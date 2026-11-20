//! Installs plugins from GitHub repositories with uvenv, and configures,
//! updates and removes them again. Plugins installed before uvenv were
//! installed with `uv tool`, and are removed with it.
//!
//! A plugin repository is a Python package with one console script that
//! takes the standard subcommands (`serve`, `install --non-interactive`,
//! `configure`, `status` and `uninstall`; see `docs/plugins.md`). Its
//! `pyproject.toml` is read before anything is installed, so the launcher
//! can say what is about to run. What was installed this way is kept in
//! `$XDG_DATA_HOME/tileception/installed-plugins.toml`, by plugin ID.
//!
//! A repository without a `pyproject.toml` installs from its release
//! instead: the archive for this machine, checked against the SHA-256 the
//! release publishes and unpacked like Doctor's binary agents, holding one
//! program that takes the same subcommands.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::channel;

use crate::Tileception;
use crate::plugins::Prompt;

/// How long installing or removing a package with uvenv or uv may take.
const PACKAGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long a plugin's own `install` may take: some download a model.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long `status` and `uninstall` may take.
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);
/// How long output is still read after a program ended, while something it
/// left running holds its pipes open.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// The largest `pyproject.toml` read.
const MAX_PYPROJECT: &str = "1000000";
/// The largest description of a release, or list of checksums, read.
const MAX_RELEASE: &str = "4000000";
/// How deep in a release's archive its program is looked for.
const PROGRAM_DEPTH: usize = 3;

/// How git reaches a repository: anonymously over https, or over ssh with
/// the user's own keys, which is how a private repository is reached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Https,
    Ssh,
}

/// A GitHub repository, optionally at a tag, branch or commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub owner: String,
    pub repo: String,
    pub rev: Option<String>,
}

impl Source {
    /// Reads `owner/repo`, `owner/repo@v1.2.0` or a github.com URL.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let mut rest = text;
        for prefix in ["git+https://", "https://", "http://"] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        rest = rest.strip_prefix("www.").unwrap_or(rest);
        rest = rest.strip_prefix("github.com/").unwrap_or(rest);
        let (path, rev) = match rest.rsplit_once('@') {
            Some((path, rev)) => (path, Some(rev)),
            None => (rest, None),
        };
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/');
        let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("{text} is not owner/repo or a GitHub repository URL"));
        };
        let name = |part: &str| {
            !part.is_empty()
                && !part.starts_with('.')
                && part.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_.".contains(letter))
        };
        if !name(owner) || !name(repo) {
            return Err(format!("{text} is not owner/repo or a GitHub repository URL"));
        }
        if let Some(rev) = rev
            && (rev.is_empty()
                || rev.contains("..")
                || rev.starts_with(['-', '/'])
                || !rev.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_./".contains(letter)))
        {
            return Err(format!("{rev} is not a tag, branch or commit"));
        }
        Ok(Self { owner: owner.into(), repo: repo.into(), rev: rev.map(Into::into) })
    }

    /// The URL of a file in the repository at a commit.
    fn raw_url(&self, commit: &str, file: &str) -> String {
        format!("https://raw.githubusercontent.com/{}/{}/{commit}/{file}", self.owner, self.repo)
    }

    /// The repository as git takes it.
    fn clone_url(&self, transport: Transport) -> String {
        match transport {
            Transport::Https => format!("https://github.com/{}/{}", self.owner, self.repo),
            Transport::Ssh => format!("ssh://git@github.com/{}/{}.git", self.owner, self.repo),
        }
    }

    /// The repository at a commit, as pip and uv take it.
    fn git_url(&self, commit: &str, transport: Transport) -> String {
        format!("git+{}@{commit}", self.clone_url(transport))
    }

    /// `owner/repo`, without the revision.
    fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// Whether both name the same repository, whatever the revision: GitHub
    /// names do not depend on case.
    fn same_repository(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.repo.eq_ignore_ascii_case(&other.repo)
    }
}

/// Whether `text` is a full commit hash.
fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|letter| letter.is_ascii_hexdigit())
}

/// The first seven characters of a commit, as GitHub shows it.
pub fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The refs `git ls-remote` is asked for to find `rev`: the default branch
/// without one, else a tag (and the commit an annotated tag points at) or a
/// branch of that exact name.
fn ls_remote_patterns(rev: Option<&str>) -> Vec<String> {
    match rev {
        None => vec!["HEAD".into()],
        Some(rev) => vec![format!("refs/tags/{rev}"), format!("refs/tags/{rev}^{{}}"), format!("refs/heads/{rev}")],
    }
}

/// The commit `git ls-remote` says `rev` is at, from its output. A tag wins
/// over a branch of the same name, as it does for git, and an annotated tag
/// gives the commit it points at rather than the tag object.
fn pick_commit(listing: &str, rev: Option<&str>) -> Option<String> {
    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(commit, name)| (commit.trim(), name.trim()))
        .filter(|(commit, _)| is_commit(commit))
        .collect();
    let wanted = match rev {
        None => vec!["HEAD".to_string()],
        Some(rev) => vec![format!("refs/tags/{rev}^{{}}"), format!("refs/tags/{rev}"), format!("refs/heads/{rev}")],
    };
    wanted
        .iter()
        .find_map(|wanted| refs.iter().find(|(_, name)| name == wanted).map(|(commit, _)| commit.to_ascii_lowercase()))
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.repo)?;
        if let Some(rev) = &self.rev {
            write!(formatter, "@{rev}")?;
        }
        Ok(())
    }
}

/// What a repository's `pyproject.toml` says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The Python package name.
    pub name: String,
    pub version: String,
    pub description: String,
    /// The console script that takes the plugin subcommands.
    pub script: String,
    /// The plugin ID, which names its manifest.
    pub plugin_id: String,
    /// Extras installed with it.
    pub extras: Vec<String>,
    /// What it needs on the machine, in words, shown before installing.
    pub needs: Vec<String>,
    /// Its `requires-python`, passed to uv so it finds or fetches a Python.
    pub python: Option<String>,
}

#[derive(Deserialize)]
struct Pyproject {
    project: Option<Project>,
    #[serde(default)]
    tool: Tools,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Project {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    requires_python: Option<String>,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct Tools {
    #[serde(default)]
    tileception: Option<Metadata>,
}

/// `[tool.tileception]`.
#[derive(Deserialize, Default)]
struct Metadata {
    #[serde(rename = "id")]
    plugin_id: Option<String>,
    script: Option<String>,
    #[serde(default)]
    extras: Vec<String>,
    #[serde(default)]
    needs: Vec<String>,
}

/// Reads a plugin's `pyproject.toml`.
pub fn package(text: &str) -> Result<Package, String> {
    let pyproject: Pyproject = toml::from_str(text).map_err(|error| format!("pyproject.toml: {}", error.message()))?;
    let project = pyproject.project.ok_or("pyproject.toml has no [project]: plugins install as Python packages")?;
    let metadata = pyproject.tool.tileception.unwrap_or_default();
    let word = |value: &str| {
        !value.is_empty() && value.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_.".contains(letter))
    };
    if !word(&project.name) {
        return Err(format!("{} is not a package name", project.name));
    }
    let script = match metadata.script {
        Some(script) => script,
        None if project.scripts.len() == 1 => project.scripts.keys().next().cloned().unwrap_or_default(),
        None if project.scripts.contains_key(&project.name) => project.name.clone(),
        None => {
            return Err(format!("{} has no single console script; name it in [tool.tileception] script", project.name));
        }
    };
    if !word(&script) {
        return Err(format!("{script} is not a script name"));
    }
    let plugin_id = metadata.plugin_id.unwrap_or_else(|| {
        let name = project.name.to_lowercase();
        name.strip_prefix("tileception-").unwrap_or(&name).to_string()
    });
    if !word(&plugin_id) || plugin_id == "plugins" || plugin_id.contains('.') {
        return Err(format!("{plugin_id} cannot be a plugin ID"));
    }
    if let Some(extra) = metadata.extras.iter().find(|extra| !word(extra)) {
        return Err(format!("{extra} is not an extra"));
    }
    if let Some(python) = &project.requires_python
        && !python.chars().all(|letter| letter.is_ascii_alphanumeric() || " <>=!~.,*".contains(letter))
    {
        return Err(format!("{python} is not a Python version"));
    }
    Ok(Package {
        name: project.name,
        version: project.version.unwrap_or_default(),
        description: project.description.unwrap_or_default(),
        script,
        plugin_id,
        extras: metadata.extras,
        needs: metadata.needs,
        python: project.requires_python,
    })
}

/// A package as looked up: what its `pyproject.toml` says at the commit it
/// was read from. That commit is what installs, so what is installed is what
/// was shown, whatever the repository does in the meantime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub package: Package,
    /// The full commit hash.
    pub commit: String,
    /// How the repository was reached, and is installed from.
    pub transport: Transport,
    /// The release archive that installs, for a repository that is not a
    /// Python package. `package` then describes that archive, and `commit`
    /// is the one the release's tag points at.
    pub release: Option<Release>,
}

/// The archive of a release that fits this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// The archive's file name.
    pub asset: String,
    pub url: String,
    /// What the archive must hash to, as the release says.
    pub sha256: String,
}

/// What GitHub says about a release.
#[derive(Debug, Deserialize)]
struct ReleaseListing {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`, on assets uploaded since GitHub started hashing them.
    #[serde(default)]
    digest: Option<String>,
}

/// Where an archive's checksum comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Checksum {
    /// GitHub's own digest of the asset.
    Known(String),
    /// A file of the same release that lists it: its URL, and whether it
    /// is about this archive alone.
    Listed { url: String, own: bool },
}

/// Where GitHub's API and its downloads are. A debug build can be pointed at
/// a stand-in, to try installing without publishing a release.
fn github() -> (String, String) {
    #[cfg(debug_assertions)]
    if let Ok(stand_in) = std::env::var("TILECEPTION_GITHUB_STAND_IN") {
        let stand_in = stand_in.trim_end_matches('/');
        return (format!("{stand_in}/api"), stand_in.to_string());
    }
    ("https://api.github.com".into(), "https://github.com".into())
}

/// The API's address of a repository's latest release, or the one of a tag.
fn release_url(api: &str, source: &Source) -> String {
    let repository = format!("{api}/repos/{}/{}/releases", source.owner, source.repo);
    match &source.rev {
        Some(tag) => format!("{repository}/tags/{tag}"),
        None => format!("{repository}/latest"),
    }
}

/// The ways release archives spell a machine architecture.
fn architecture_names(architecture: &str) -> Vec<&str> {
    match architecture {
        "x86_64" => vec!["x86_64", "amd64", "x64"],
        "aarch64" => vec!["aarch64", "arm64"],
        // Not `x86`, which `x86_64` holds as a part of its own.
        "x86" => vec!["i686", "i586", "i386"],
        other => vec![other],
    }
}

/// Whether a file name holds `word` as a part of its own: `arm` is not in
/// `arm64`, and `linux` is in `tool_Linux_amd64`.
fn names_part(name: &str, word: &str) -> bool {
    let name = name.to_lowercase();
    name.match_indices(word).any(|(start, _)| {
        let before = name[..start].chars().next_back();
        let after = name[start + word.len()..].chars().next();
        !before.is_some_and(|letter| letter.is_ascii_alphanumeric())
            && !after.is_some_and(|letter| letter.is_ascii_alphanumeric())
    })
}

/// The archive among a release's files for Linux on `architecture`. One
/// built for glibc is preferred, then one that does not say, then musl,
/// which runs anywhere; among those, the plain build with the shortest name.
fn pick_asset<'assets>(assets: &'assets [Asset], architecture: &str) -> Option<&'assets Asset> {
    let libc = |name: &str| {
        if names_part(name, "gnu") || names_part(name, "glibc") {
            0
        } else if names_part(name, "musl") {
            2
        } else {
            1
        }
    };
    // A build for newer processors only, or one for debugging, loses to the
    // plain one; so does a longer name, which says more than is asked for.
    let variant = |name: &str| {
        ["v2", "v3", "v4", "debug", "dbg", "debuginfo", "symbols", "dsym"].iter().any(|word| names_part(name, word))
    };
    assets
        .iter()
        .filter(|asset| tileception_acp::install::is_archive(&asset.name))
        .filter(|asset| names_part(&asset.name, "linux") && !names_part(&asset.name, "android"))
        .filter(|asset| architecture_names(architecture).iter().any(|word| names_part(&asset.name, word)))
        .min_by_key(|asset| (libc(&asset.name), variant(&asset.name), asset.name.len(), asset.name.clone()))
}

/// A SHA-256 in hex, lowercase, or nothing when `text` is not one.
fn sha256_hex(text: &str) -> Option<String> {
    (text.len() == 64 && text.chars().all(|letter| letter.is_ascii_hexdigit())).then(|| text.to_ascii_lowercase())
}

/// Where the release says what `archive` hashes to: GitHub's digest of it,
/// else `<archive>.sha256`, else a list of checksums.
fn pick_checksum(assets: &[Asset], archive: &Asset) -> Option<Checksum> {
    if let Some(digest) = archive.digest.as_deref().and_then(|digest| digest.strip_prefix("sha256:"))
        && let Some(sha256) = sha256_hex(digest)
    {
        return Some(Checksum::Known(sha256));
    }
    let lower = |asset: &Asset| asset.name.to_lowercase();
    let own =
        [format!("{}.sha256", archive.name), format!("{}.sha256sum", archive.name)].map(|name| name.to_lowercase());
    if let Some(asset) = assets.iter().find(|asset| own.contains(&lower(asset))) {
        return Some(Checksum::Listed { url: asset.browser_download_url.clone(), own: true });
    }
    // A list that says it is SHA-256 comes before one that does not say,
    // and one of another kind of hash is no use.
    let list = |asset: &&Asset| {
        let name = lower(asset);
        let signature =
            [".sig", ".asc", ".pem", ".minisig", ".sigstore", ".json"].iter().any(|ending| name.ends_with(ending));
        let other = ["sha512", "sha384", "sha224", "sha1", "md5", "blake", "b2sum", "b3sum"]
            .iter()
            .any(|hash| name.contains(hash));
        let says = name.contains("sha256");
        let fits = !signature && (name.contains("checksums") || name.contains("sha256sums") || name == "sha256sum.txt");
        (fits && (says || !other)).then_some((!says, name))
    };
    assets
        .iter()
        .filter_map(|asset| list(&asset).map(|rank| (rank, asset)))
        .min_by(|(one, _), (other, _)| one.cmp(other))
        .map(|(_, asset)| Checksum::Listed { url: asset.browser_download_url.clone(), own: false })
}

/// The checksum of `archive` in a checksum file, as `sha256sum` writes them:
/// `<hex>  <name>` on a line. A file about one archive alone may hold just
/// the hash.
fn checksum_in(text: &str, archive: &str, own: bool) -> Option<String> {
    let listed = text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let (hash, name) = (words.next()?, words.next()?);
        let name = name.trim_start_matches('*').trim_start_matches("./");
        (name == archive).then(|| sha256_hex(hash)).flatten()
    });
    listed.or_else(|| if own { sha256_hex(text.trim()) } else { None })
}

/// What installs from a release: its tag, the archive for this machine and
/// where that archive's checksum is. `listing` is GitHub's JSON.
fn choose_release(source: &Source, listing: &str, architecture: &str) -> Result<(String, Asset, Checksum), String> {
    let listing: ReleaseListing =
        serde_json::from_str(listing).map_err(|error| format!("its release could not be read: {error}"))?;
    let tag = listing.tag_name;
    let rev_like = Source::parse(&format!("{}@{tag}", source.repository())).is_ok();
    if !rev_like {
        return Err(format!("its release has a tag that cannot be installed: {tag}"));
    }
    let archive = pick_asset(&listing.assets, architecture).ok_or_else(|| {
        let names: Vec<&str> = listing.assets.iter().map(|asset| asset.name.as_str()).collect();
        let has = if names.is_empty() { "no files".to_string() } else { names.join(", ") };
        format!("its release {tag} has no archive for Linux on {architecture} (it has {has})")
    })?;
    let checksum = pick_checksum(&listing.assets, archive).ok_or_else(|| {
        format!(
            "its release {tag} publishes no SHA-256 checksum for {}: the archive cannot be checked, so it is not installed",
            archive.name
        )
    })?;
    Ok((tag, archive.clone(), checksum))
}

/// Whether `url` is a file of one of the repository's releases, which is all
/// that is downloaded for it.
fn release_file(downloads: &str, source: &Source, url: &str) -> bool {
    let prefix = format!("{downloads}/{}/{}/releases/download/", source.owner, source.repo);
    url.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(&prefix))
        && !url[prefix.len()..].split('/').any(|part| part.is_empty() || part == "..")
}

/// A release archive described like a Python package: named after the
/// repository, as its program is, with the tag as its version.
fn release_package(source: &Source, release: &Release) -> Result<Package, String> {
    let name = source.repo.clone();
    let lower = name.to_lowercase();
    let plugin_id = lower.strip_prefix("tileception-").unwrap_or(&lower).to_string();
    if plugin_id.is_empty() || plugin_id == "plugins" || plugin_id.contains('.') {
        return Err(format!("{plugin_id} cannot be a plugin ID"));
    }
    Ok(Package {
        script: name.clone(),
        name,
        version: release.tag.clone(),
        description: format!("{} from its release", release.asset),
        plugin_id,
        extras: Vec::new(),
        needs: Vec::new(),
        python: None,
    })
}

/// Where a plugin's release archive is unpacked.
fn release_dir(plugin_id: &str) -> PathBuf {
    data_dir().join("plugin-releases").join(plugin_id)
}

/// The program in an unpacked release: the executable file named `name`,
/// the one nearest the top, else the only executable file there is.
fn find_program(dir: &Path, name: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let mut programs = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            // Links are not followed: the program is a file of the archive.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && depth < PROGRAM_DEPTH {
                folders.push((entry.path(), depth + 1));
            } else if kind.is_file() && entry.metadata().is_ok_and(|file| file.permissions().mode() & 0o111 != 0) {
                programs.push((depth, entry.path()));
            }
        }
    }
    programs.sort();
    let named = programs.iter().find(|(_, path)| path.file_name().is_some_and(|file| file == name));
    match (named, &programs[..]) {
        (Some((_, path)), _) | (None, [(_, path)]) => Ok(path.clone()),
        (None, []) => Err(format!("the archive holds no program: it needs an executable file called {name}")),
        (None, _) => Err(format!("the archive holds several programs and none is called {name}")),
    }
}

/// Whether what is installed is what a fresh lookup found, so an update has
/// nothing to do.
fn is_current(installed: &Installed, lookup: &Lookup) -> bool {
    let release = lookup.release.as_ref();
    installed.commit.as_deref() == Some(lookup.commit.as_str())
        && installed.release.as_deref() == release.map(|release| release.tag.as_str())
        && release.is_none_or(|release| installed.sha256.as_deref() == Some(release.sha256.as_str()))
}

/// The requirement `uvenv install` gets, extras and all.
fn requirement(source: &Source, lookup: &Lookup) -> String {
    let package = &lookup.package;
    let extras = if package.extras.is_empty() { String::new() } else { format!("[{}]", package.extras.join(",")) };
    format!("{}{extras} @ {}", package.name, source.git_url(&lookup.commit, lookup.transport))
}

fn install_args(source: &Source, lookup: &Lookup) -> Vec<String> {
    let mut args = vec!["install".to_string()];
    if let Some(python) = &lookup.package.python {
        args.extend(["--python".to_string(), python.trim().to_string()]);
    }
    args.push(requirement(source, lookup));
    args
}

/// Why a release was not swapped in, and whether what was there is back.
#[derive(Debug, PartialEq, Eq)]
struct Swap {
    error: String,
    restored: bool,
}

/// Puts the unpacked release `staged` at `dir`, where the installed version
/// was until `aside` took it, and lets `finish` set it up. The installed
/// version is deleted only once that worked; otherwise it comes back and
/// the new one goes. A first install that fails stays for another try.
fn swap_in<Done>(
    staged: &Path,
    dir: &Path,
    aside: Option<Aside>,
    finish: impl FnOnce() -> Result<Done, String>,
) -> Result<Done, Swap> {
    let placed = dir
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::rename(staged, dir))
        .map_err(|error| format!("{}: {error}", dir.display()));
    let failed = match placed.and_then(|()| finish()) {
        Ok(done) => {
            if let Some(aside) = aside {
                aside.discard();
            }
            return Ok(done);
        }
        Err(error) => error,
    };
    let Some(aside) = aside else {
        let _ = std::fs::remove_dir_all(staged);
        return Err(Swap { error: failed, restored: false });
    };
    let _ = std::fs::remove_dir_all(staged);
    let _ = std::fs::remove_dir_all(dir);
    match aside.restore() {
        Ok(()) => Err(Swap { error: failed, restored: true }),
        Err(stuck) => Err(Swap {
            error: format!("{failed}; the installed version could not be put back: {stuck}"),
            restored: false,
        }),
    }
}

/// Puts back what updates that never ended left set aside, for every
/// installed plugin.
fn recover_set_aside(record: &Record) {
    for plugin_id in record.plugins.keys() {
        match Aside::recover(&data_dir().join("set-aside").join(plugin_id)) {
            Ok(true) => {
                tracing::info!("put the plugin {plugin_id} back, which an update that never ended had set aside")
            }
            Ok(false) => {}
            Err(error) => tracing::warn!("{plugin_id}: {error}"),
        }
    }
}

/// A plugin installed from a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// `owner/repo`, with `@rev` when one was asked for: what `update`
    /// looks up again.
    pub source: String,
    /// The commit installed. Only `update` moves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The Python package, for uninstalling.
    pub package: String,
    /// What installed it. Records from before uvenv say nothing: uv.
    #[serde(default)]
    pub tool: Tool,
    /// The script's absolute path.
    pub command: PathBuf,
    /// Whether the manifest was made by installing (by Tileception or the
    /// plugin's `install`), so removing may delete it. A manifest that was
    /// there before stays.
    #[serde(default)]
    pub wrote_manifest: bool,
    /// The tag of the release it was unpacked from, for a plugin that is
    /// not a Python package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// The SHA-256 of that release's archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// The program that installed a plugin's package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(clippy::enum_variant_names, reason = "uv's own subcommand is called `uv tool`")]
pub enum Tool {
    /// `uv tool install`, which installed plugins before uvenv did.
    #[default]
    #[serde(rename = "uv")]
    UvTool,
    Uvenv,
    /// Nothing: the archive of a release, unpacked by Tileception.
    Release,
}

impl Tool {
    /// The program that removes the package; a release needs none.
    fn program(self) -> Option<&'static str> {
        match self {
            Tool::UvTool => Some("uv"),
            Tool::Uvenv => Some("uvenv"),
            Tool::Release => None,
        }
    }

    fn uninstall_args(self, package: &str) -> Vec<&str> {
        match self {
            Tool::UvTool => vec!["tool", "uninstall", package],
            Tool::Uvenv => vec!["uninstall", package],
            Tool::Release => Vec::new(),
        }
    }
}

/// Every installed plugin, by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub plugins: BTreeMap<String, Installed>,
}

/// A package name as Python compares it, and as uvenv names its
/// environments: lowercase, with every run of `-`, `_` and `.` one `-`.
fn normal_package(name: &str) -> String {
    let mut normal = String::with_capacity(name.len());
    for letter in name.chars() {
        if matches!(letter, '-' | '_' | '.') {
            if !normal.ends_with('-') {
                normal.push('-');
            }
        } else {
            normal.extend(letter.to_lowercase());
        }
    }
    normal
}

fn same_package(one: &str, other: &str) -> bool {
    normal_package(one) == normal_package(other)
}

/// Where uvenv links the scripts it installs.
fn uvenv_bin_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/bin")
}

/// Where uvenv keeps an environment for each package.
fn uvenv_venv_dir(package: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/uvenv/venvs").join(normal_package(package))
}

/// Where `uv tool install` kept a package, as uv finds it by default.
fn uv_tool_dir(package: &str) -> PathBuf {
    let tools = std::env::var_os("UV_TOOL_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from).join("uv/tools")
    });
    tools.join(normal_package(package))
}

/// What `uvenv list --json` says about `package`, if uvenv has it.
fn uvenv_entry(listing: &[u8], package: &str) -> Option<serde_json::Value> {
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_slice(listing) else {
        return None;
    };
    entries.into_iter().find(|entry| {
        entry.get("name").and_then(serde_json::Value::as_str).is_some_and(|name| same_package(name, package))
    })
}

/// The scripts uvenv linked into its bin folder for an entry. It skips a
/// script whose name another program has there, and still succeeds.
fn uvenv_linked_scripts(entry: &serde_json::Value) -> Vec<String> {
    entry
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .map(|scripts| {
            scripts.iter().filter(|(_, linked)| linked.as_bool() == Some(true)).map(|(name, _)| name.clone()).collect()
        })
        .unwrap_or_default()
}

/// The links `uv tool install` made, from the receipt in its tool folder.
fn uv_tool_links(receipt: &str) -> Vec<PathBuf> {
    let Ok(receipt) = toml::from_str::<toml::Value>(receipt) else {
        return Vec::new();
    };
    receipt
        .get("tool")
        .and_then(|tool| tool.get("entrypoints"))
        .and_then(toml::Value::as_array)
        .map(|entrypoints| {
            entrypoints
                .iter()
                .filter_map(|entrypoint| entrypoint.get("install-path")?.as_str())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// A link that points into `dir`, so it is the environment's own and not
/// another program's.
fn links_into(link: &Path, dir: &Path) -> bool {
    std::fs::read_link(link).is_ok_and(|target| target.starts_with(dir))
}

/// The list of what [`Aside`] moved, in its folder.
const MOVED: &str = "moved.json";

/// An installed plugin's environment and links, moved out of the way while
/// its replacement installs, so a failed update can put them back.
struct Aside {
    /// Each original path, and where it is now.
    moved: Vec<(PathBuf, PathBuf)>,
    folder: PathBuf,
}

impl Aside {
    /// Moves `paths` into `folder`, or moves nothing. What an earlier run
    /// left there is put back first, where nothing has replaced it.
    fn take(folder: PathBuf, paths: Vec<PathBuf>) -> Result<Self, String> {
        Self::recover(&folder)?;
        std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        let mut aside = Aside { moved: Vec::new(), folder };
        let mut listed: Vec<(usize, PathBuf)> = Vec::new();
        for (index, path) in paths.into_iter().enumerate() {
            if std::fs::symlink_metadata(&path).is_err() {
                continue;
            }
            let stored = aside.folder.join(index.to_string());
            // Listed before it moves, so a run that ends in between can
            // still be undone.
            listed.push((index, path.clone()));
            let moved = serde_json::to_string(&listed)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    std::fs::write(aside.folder.join(MOVED), text)
                        .map_err(|error| format!("{}: {error}", aside.folder.display()))
                })
                .and_then(|()| std::fs::rename(&path, &stored).map_err(|error| error.to_string()));
            if let Err(error) = moved {
                let error = format!("Could not set {} aside: {error}", path.display());
                aside.restore()?;
                return Err(error);
            }
            aside.moved.push((path, stored));
        }
        Ok(aside)
    }

    /// Puts back what a run that never ended left in `folder`, where nothing
    /// is in its place, and removes the rest, which was replaced since. Says
    /// whether anything came back. What cannot be put back stays, and is an
    /// error.
    fn recover(folder: &Path) -> Result<bool, String> {
        if std::fs::symlink_metadata(folder).is_err() {
            return Ok(false);
        }
        let kept = || {
            format!("{} holds what an update that never ended set aside; put it back or remove it", folder.display())
        };
        let listed: Vec<(usize, PathBuf)> = match std::fs::read_to_string(folder.join(MOVED)) {
            Ok(text) => serde_json::from_str(&text).map_err(|_| kept())?,
            // Nothing listed was moved: an earlier version's folder, or one
            // that ended before it moved anything.
            Err(_) if std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none()) => Vec::new(),
            Err(_) => return Err(kept()),
        };
        let mut back = false;
        for (index, path) in listed.iter().rev() {
            let stored = folder.join(index.to_string());
            if std::fs::symlink_metadata(&stored).is_ok() && std::fs::symlink_metadata(path).is_err() {
                path.parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::rename(&stored, path))
                    .map_err(|error| format!("{}: {error}; {}", path.display(), kept()))?;
                back = true;
            }
        }
        std::fs::remove_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        if let Some(parent) = folder.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        Ok(back)
    }

    /// Puts everything back where it was.
    fn restore(self) -> Result<(), String> {
        let mut failed = Vec::new();
        for (path, stored) in self.moved.iter().rev() {
            if let Err(error) = std::fs::rename(stored, path) {
                failed.push(format!("{} is still at {} ({error})", path.display(), stored.display()));
            }
        }
        if failed.is_empty() {
            let _ = std::fs::remove_dir_all(&self.folder);
            self.remove_parent();
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// Deletes what was moved: the replacement is in place.
    fn discard(self) {
        if let Err(error) = std::fs::remove_dir_all(&self.folder) {
            tracing::warn!("{}: {error}", self.folder.display());
        }
        self.remove_parent();
    }

    /// The folder for every plugin's, once it holds none.
    fn remove_parent(&self) {
        if let Some(parent) = self.folder.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Refuses an install that would take over another plugin: one with the same
/// ID from another repository, whose record it would overwrite, or one with
/// the same package under another ID, whose environment it would replace.
/// Installing the same repository again, at any revision, is fine.
fn conflict(record: &Record, source: &Source, package: &Package) -> Result<(), String> {
    if let Some(installed) = record.plugins.get(&package.plugin_id) {
        let same = Source::parse(&installed.source).is_ok_and(|other| other.same_repository(source));
        if !same {
            let from = Source::parse(&installed.source).map_or(installed.source.clone(), |other| other.repository());
            return Err(format!("plugin ID {} is taken by {from}; remove it first", package.plugin_id));
        }
    }
    if let Some((plugin_id, installed)) = record.plugins.iter().find(|(plugin_id, installed)| {
        **plugin_id != package.plugin_id && same_package(&installed.package, &package.name)
    }) {
        return Err(format!("{} is already installed as the plugin {plugin_id}; remove it first", installed.package));
    }
    Ok(())
}

fn data_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("tileception")
}

fn record_path() -> PathBuf {
    data_dir().join("installed-plugins.toml")
}

fn manifest_path(plugin_id: &str) -> PathBuf {
    data_dir().join("plugins").join(format!("{plugin_id}.toml"))
}

/// The manifest Tileception writes when a plugin's `install` wrote none.
fn minimal_manifest(command: &Path) -> String {
    let quoted = toml::Value::String(command.display().to_string());
    format!("command = {quoted}\nargs = [\"serve\"]\n")
}

/// Deletes a removed plugin's manifest if installing made it, and says so
/// when it stays. Older records do not say who made it; the manifest
/// Tileception writes itself is recognised by its text.
fn remove_manifest(installed: &Installed, manifest: &Path) -> Result<Option<String>, String> {
    if !manifest.exists() {
        return Ok(None);
    }
    let ours = installed.wrote_manifest
        || std::fs::read_to_string(manifest).is_ok_and(|text| text == minimal_manifest(&installed.command));
    if !ours {
        return Ok(Some(format!("kept {}, which was there before it was installed", manifest.display())));
    }
    std::fs::remove_file(manifest).map_err(|error| format!("{}: {error}", manifest.display()))?;
    Ok(None)
}

impl Record {
    pub fn load() -> Self {
        Self::read(&record_path())
    }

    fn read(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("{}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save(&self) -> Result<(), String> {
        let path = record_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let text = toml::to_string(self).map_err(|error| error.to_string())?;
        let partial = path.with_extension("toml.partial");
        std::fs::write(&partial, text).map_err(|error| format!("{}: {error}", partial.display()))?;
        std::fs::rename(&partial, &path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Work done off the event loop.
enum Job {
    Look(Source),
    /// Asks an installed plugin how it is set up.
    Configure(String),
    /// Looks up and installs without showing it first.
    Add(Source),
    Install(Source, Box<Lookup>),
    Update(Vec<String>),
    Remove(String),
}

/// What a job reports back.
pub enum Report {
    Looked {
        source: Source,
        result: Box<Result<Lookup, String>>,
    },
    /// The running job moved on to another step, in words.
    Step(String),
    Done {
        title: String,
        result: Result<String, String>,
        configure: Vec<Configure>,
    },
    /// How to set up a plugin asked for with `plugins configure`.
    Configure(Result<Configure, String>),
}

/// A plugin to set up, in the launcher with its setup prompt when it has
/// one, else with its `configure` in a terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct Configure {
    plugin_id: String,
    setup: Option<Prompt>,
}

/// The launcher's view of installing.
#[derive(Default)]
pub struct Installer {
    sender: Option<channel::Sender<Report>>,
    /// What runs now, in words, such as "Installing tileception-odoo".
    pub busy: Option<String>,
    /// The last repository looked up, and what it said.
    pub looked: Option<(Source, Result<Lookup, String>)>,
    /// The repository being looked up.
    pub looking: Option<Source>,
    /// What is installed, read again after every change.
    pub record: Record,
}

/// `tileception msg plugins list`: one line per installed plugin.
pub fn listing() -> String {
    let record = Record::load();
    if record.plugins.is_empty() {
        return "No plugins were installed from a repository\n".into();
    }
    record
        .plugins
        .iter()
        .map(|(plugin_id, installed)| {
            let commit = installed.commit.as_deref().map_or("-", short);
            format!("{plugin_id}\t{}\t{commit}\t{}\n", installed.source, installed.command.display())
        })
        .collect()
}

/// Whether reading a file failed because the repository does not have it,
/// as curl says of GitHub's answer and git of a commit, and not because
/// GitHub could not be reached.
fn no_such_file(error: &str) -> bool {
    error.contains("error: 404") || error.contains("does not exist in") || error.contains("exists on disk, but not in")
}

/// A file of its own for each lookup: two can run at once, and one must
/// not read what the other fetched.
fn lookup_file() -> String {
    static LOOKUPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let lookup = LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("plugin-pyproject-{}-{lookup}.toml", std::process::id())
}

/// Runs `command` without a terminal on stdin, gathering its output, and
/// ends it after `timeout`.
fn run(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    // Output is gathered as it comes, and each reader says when its pipe
    // closed. Something the program started in the background may hold the
    // pipes open long after it ended, so they are not waited for forever.
    let (closed, pipes_closed) = std::sync::mpsc::channel::<()>();
    let gather = |pipe: Option<Box<dyn Read + Send>>| {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let gathered = bytes.clone();
        let closed = closed.clone();
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0; 8192];
                while let Ok(length @ 1..) = pipe.read(&mut chunk) {
                    gathered
                        .lock()
                        .expect("output buffer mutex must not be poisoned")
                        .extend_from_slice(&chunk[..length]);
                }
            }
            let _ = closed.send(());
        });
        bytes
    };
    let stdout = gather(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = gather(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    drop(closed);
    let take = |bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("output buffer mutex must not be poisoned"))
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| format!("{program}: {error}"))? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} took longer than {} minutes", timeout.as_secs().div_ceil(60)));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let grace = Instant::now() + OUTPUT_GRACE;
    for _ in 0..2 {
        if pipes_closed.recv_timeout(grace.saturating_duration_since(Instant::now())).is_err() {
            break;
        }
    }
    Ok(Output { status, stdout: take(stdout), stderr: take(stderr) })
}

/// The last lines a failed program wrote, for a notification, without the
/// colours and progress lines uvenv writes even when it has no terminal.
fn tail(output: &Output) -> String {
    let text = if output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let text = without_escapes(&text);
    // A progress line is rewritten in place: only what came last shows.
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.rsplit('\r').next())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..].join("\n")
}

/// Text without terminal escape sequences (`ESC [ ... letter`).
fn without_escapes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut letters = text.chars();
    while let Some(letter) = letters.next() {
        if letter != '\u{1b}' {
            plain.push(letter);
        } else if letters.next() == Some('[') {
            // Parameters and intermediates, up to the final byte.
            for letter in letters.by_ref() {
                if ('@'..='~').contains(&letter) {
                    break;
                }
            }
        }
    }
    plain
}

/// Runs a job's programs with the session's environment.
struct Runner {
    environment: Vec<(OsString, Option<OsString>)>,
    /// Where steps are reported, so the launcher can say what runs now.
    sender: channel::Sender<Report>,
}

impl Runner {
    fn step(&self, step: String) {
        let _ = self.sender.send(Report::Step(step));
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        for (key, value) in &self.environment {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command
    }

    /// Runs and fails with what it wrote when it does not succeed.
    fn check(&self, what: &str, command: Command, timeout: Duration) -> Result<Output, String> {
        let output = run(command, timeout)?;
        if output.status.success() { Ok(output) } else { Err(format!("{what} failed: {}", tail(&output))) }
    }

    /// The program a plugin's package is installed and removed with.
    fn find(&self, tool: Tool) -> Result<PathBuf, String> {
        let program = tool.program().ok_or("A plugin from a release is removed without another program")?;
        tileception_acp::local::find(program).ok_or_else(|| match tool {
            Tool::Uvenv => "Plugins are installed and removed with uvenv, which is not installed: run tileception setup to install it, \
                            or see https://github.com/robinvandernoord/uvenv"
                .into(),
            Tool::UvTool | Tool::Release => {
                "This plugin was installed with uv, which is not installed anymore".into()
            }
        })
    }

    /// The commit `source` is at now: its default branch, or the tag,
    /// branch or full commit it names.
    fn resolve(&self, source: &Source) -> Result<(String, Transport), String> {
        let given = source.rev.as_deref().filter(|rev| is_commit(rev)).map(str::to_ascii_lowercase);
        // A commit named in full is not looked up, but which way the
        // repository can be reached still is.
        let patterns =
            if given.is_some() { vec!["HEAD".to_string()] } else { ls_remote_patterns(source.rev.as_deref()) };
        let (listing, transport) = match self.ls_remote(source, Transport::Https, &patterns)? {
            Ok(listing) => (listing, Transport::Https),
            // GitHub asks for a login rather than say a repository is
            // missing: a private one can still be reached over ssh.
            Err(error) if error.contains("could not read Username") => {
                match self.ls_remote(source, Transport::Ssh, &patterns)? {
                    Ok(listing) => (listing, Transport::Ssh),
                    Err(_) => {
                        return Err(format!(
                            "{} is private or does not exist: a private repository is reached over ssh, \
                             with a key that `ssh -T git@github.com` uses without asking (in your ssh agent)",
                            source.repository()
                        ));
                    }
                }
            }
            Err(error) => return Err(format!("Could not ask GitHub about {}: {error}", source.repository())),
        };
        if let Some(commit) = given {
            return Ok((commit, transport));
        }
        let commit = pick_commit(&listing, source.rev.as_deref()).ok_or_else(|| match &source.rev {
            Some(rev) => format!(
                "{} has no tag or branch {rev}; name a tag, a branch or a full commit hash",
                source.repository()
            ),
            None => format!("{} has no default branch", source.repository()),
        })?;
        Ok((commit, transport))
    }

    /// `git ls-remote`: what it listed, or what it said when it failed.
    /// Only a program that could not run at all is an error.
    fn ls_remote(
        &self,
        source: &Source,
        transport: Transport,
        patterns: &[String],
    ) -> Result<Result<String, String>, String> {
        let url = source.clone_url(transport);
        let mut args = vec!["ls-remote", "--", url.as_str()];
        args.extend(patterns.iter().map(String::as_str));
        let output = run(self.git(&args, transport), QUICK_TIMEOUT)
            .map_err(|error| format!("Could not ask GitHub about {} ({error})", source.repository()))?;
        Ok(if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(tail(&output))
        })
    }

    /// Git that never waits for an answer: no login prompt for https, and for
    /// ssh no passphrase or host key question, unless the user chose their
    /// own ssh command.
    fn git(&self, args: &[&str], transport: Transport) -> Command {
        let mut command = self.command("git", args);
        self.quiet_git(&mut command, transport);
        command
    }

    fn quiet_git(&self, command: &mut Command, transport: Transport) {
        command.env("GIT_TERMINAL_PROMPT", "0");
        if transport == Transport::Ssh && !self.own_ssh_command() {
            command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
    }

    /// Whether the user chose how git runs ssh, which `GIT_SSH_COMMAND`
    /// would override: in the environment or in git's config.
    fn own_ssh_command(&self) -> bool {
        let set = |name: &str| {
            self.environment
                .iter()
                .find(|(key, _)| key == name)
                .map_or_else(|| std::env::var_os(name).is_some(), |(_, value)| value.is_some())
        };
        set("GIT_SSH_COMMAND")
            || set("GIT_SSH")
            || run(self.command("git", &["config", "--get", "core.sshCommand"]), QUICK_TIMEOUT)
                .is_ok_and(|output| output.status.success())
    }

    /// `pyproject.toml` at a commit, fetched with git: the only way to read
    /// a private repository's files with nothing but ssh.
    fn git_pyproject(&self, source: &Source, commit: &str, transport: Transport, dir: &Path) -> Result<String, String> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let dir_text = dir.display().to_string();
        let url = source.clone_url(transport);
        let result = (|| {
            self.check("git init", self.git(&["init", "--quiet", "--bare", &dir_text], transport), QUICK_TIMEOUT)?;
            self.check(
                "git fetch",
                self.git(&["-C", &dir_text, "fetch", "--quiet", "--depth", "1", "--", &url, commit], transport),
                PACKAGE_TIMEOUT,
            )?;
            let shown = self.check(
                "git show",
                self.git(&["-C", &dir_text, "show", "FETCH_HEAD:pyproject.toml"], transport),
                QUICK_TIMEOUT,
            )?;
            if shown.stdout.len() > MAX_PYPROJECT.parse::<usize>().unwrap_or(usize::MAX) {
                return Err("pyproject.toml is too large".to_string());
            }
            String::from_utf8(shown.stdout).map_err(|error| error.to_string())
        })();
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    /// Finds the commit to install and reads its `pyproject.toml`, refusing
    /// a plugin that would take over another one.
    fn look(&self, source: &Source) -> Result<Lookup, String> {
        let lookup = self.read(source)?;
        conflict(&Record::load(), source, &lookup.package)?;
        Ok(lookup)
    }

    /// Finds the commit to install and reads its `pyproject.toml`.
    fn read(&self, source: &Source) -> Result<Lookup, String> {
        let (commit, transport) = self.resolve(source)?;
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
            .join("tileception");
        std::fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
        let path = cache.join(lookup_file());
        let text = match transport {
            Transport::Https => tileception_acp::curl(&source.raw_url(&commit, "pyproject.toml"), &path, MAX_PYPROJECT)
                .and_then(|()| {
                    let text = std::fs::read_to_string(&path).map_err(|error| error.to_string());
                    let _ = std::fs::remove_file(&path);
                    text
                }),
            Transport::Ssh => self.git_pyproject(source, &commit, transport, &path),
        };
        match text {
            Ok(text) => Ok(Lookup { package: package(&text)?, commit, transport, release: None }),
            // Not a Python package: its release may hold a program instead.
            Err(error) if no_such_file(&error) => {
                self.read_release(source, &commit, transport, &path).map_err(|release| {
                    format!("{} has no pyproject.toml at {}, and {release}", source.repository(), short(&commit))
                })
            }
            Err(error) => {
                Err(format!("{} has no pyproject.toml to read at {} ({error})", source.repository(), short(&commit)))
            }
        }
    }

    /// Finds the archive to install from the latest release, or from the
    /// release of the tag the source names. `scratch` is a file to use.
    fn read_release(
        &self,
        source: &Source,
        commit: &str,
        transport: Transport,
        scratch: &Path,
    ) -> Result<Lookup, String> {
        if transport == Transport::Ssh {
            return Err("its releases cannot be read: an archive is only downloaded from a public repository".into());
        }
        let (api, downloads) = github();
        let fetch = |url: &str| {
            let fetched = tileception_acp::curl(url, scratch, MAX_RELEASE)
                .and_then(|()| std::fs::read_to_string(scratch).map_err(|error| error.to_string()));
            let _ = std::fs::remove_file(scratch);
            fetched
        };
        let listing = fetch(&release_url(&api, source)).map_err(|error| match &source.rev {
            Some(rev) if no_such_file(&error) => format!("it has no release tagged {rev}"),
            None if no_such_file(&error) => "it has no release".to_string(),
            _ if error.contains("error: 403") || error.contains("error: 429") => {
                "GitHub is not answering questions about releases from this address now; try again in an hour".into()
            }
            _ => format!("its release could not be read ({error})"),
        })?;
        let (tag, archive, checksum) = choose_release(source, &listing, std::env::consts::ARCH)?;
        let inside = |url: &str| {
            if release_file(&downloads, source, url) {
                Ok(())
            } else {
                Err(format!("its release {tag} points at a file elsewhere: {url}"))
            }
        };
        inside(&archive.browser_download_url)?;
        let sha256 = match checksum {
            Checksum::Known(sha256) => sha256,
            Checksum::Listed { url, own } => {
                inside(&url)?;
                let text = fetch(&url)
                    .map_err(|error| format!("the checksums of its release {tag} could not be read ({error})"))?;
                checksum_in(&text, &archive.name, own)
                    .ok_or_else(|| format!("the checksums of its release {tag} do not list {}", archive.name))?
            }
        };
        // The commit the tag is at, which the record and the launcher show.
        let commit = if source.rev.as_deref() == Some(tag.as_str()) {
            commit.to_string()
        } else {
            self.resolve(&Source { rev: Some(tag.clone()), ..source.clone() })?.0
        };
        let release = Release { tag, asset: archive.name, url: archive.browser_download_url, sha256 };
        Ok(Lookup { package: release_package(source, &release)?, commit, transport, release: Some(release) })
    }

    /// An installed plugin's environment and the links into it.
    fn old_paths(&self, uvenv: &Path, plugin_id: &str, installed: &Installed) -> Vec<PathBuf> {
        match installed.tool {
            Tool::Uvenv => {
                let venv = uvenv_venv_dir(&installed.package);
                let normal = normal_package(&installed.package);
                let listing = run(self.command(uvenv, &["list", "--json", "--skip-updates", &normal]), QUICK_TIMEOUT)
                    .map(|output| output.stdout)
                    .unwrap_or_default();
                let scripts = uvenv_entry(&listing, &installed.package)
                    .map(|entry| uvenv_linked_scripts(&entry))
                    .unwrap_or_default();
                let links = scripts.iter().map(|script| uvenv_bin_dir().join(script));
                links.filter(|link| links_into(link, &venv)).chain([venv.clone()]).collect()
            }
            Tool::UvTool => {
                let tool = uv_tool_dir(&installed.package);
                let receipt = std::fs::read_to_string(tool.join("uv-receipt.toml")).unwrap_or_default();
                let links = uv_tool_links(&receipt).into_iter().chain([installed.command.clone()]);
                let mut paths: Vec<PathBuf> = links.filter(|link| links_into(link, &tool)).collect();
                paths.dedup();
                paths.push(tool);
                paths
            }
            Tool::Release => vec![release_dir(plugin_id)],
        }
    }

    /// Installs the package at the commit looked up, lets the plugin set
    /// itself up without asking anything, makes sure a manifest exists, and
    /// asks whether it still needs configuring.
    fn install(&self, source: &Source, lookup: &Lookup) -> Result<(String, Option<Configure>), String> {
        if let Some(release) = &lookup.release {
            return self.install_release(source, lookup, release);
        }
        let package = &lookup.package;
        // Again, as something else may have been installed since the lookup.
        conflict(&Record::load(), source, package)?;
        let uvenv = self.find(Tool::Uvenv)?;
        let mut record = Record::load();
        let before = record.plugins.get(&package.plugin_id).cloned();
        let normal = normal_package(&package.name);
        let list = |what: &str| {
            self.check(what, self.command(&uvenv, &["list", "--json", "--skip-updates", &normal]), QUICK_TIMEOUT)
        };
        // An environment uvenv has for the package is the user's own unless
        // the record says Tileception made it.
        let ours = before
            .as_ref()
            .is_some_and(|installed| installed.tool == Tool::Uvenv && same_package(&installed.package, &package.name));
        if !ours && uvenv_entry(&list("uvenv list")?.stdout, &package.name).is_some() {
            return Err(format!(
                "{} is already installed with uvenv, outside Tileception; remove it with uvenv uninstall {} first",
                package.name, package.name
            ));
        }
        // The installed version keeps working until its replacement is in
        // place: uvenv installs nothing over it, so it moves aside, and comes
        // back if installing fails.
        let aside = match &before {
            Some(installed) => {
                self.step(format!("Setting {} aside until its replacement is installed", installed.package));
                let folder = data_dir().join("set-aside").join(&package.plugin_id);
                Some(Aside::take(folder, self.old_paths(&uvenv, &package.plugin_id, installed))?)
            }
            None => None,
        };
        // Whatever is in uvenv's folder for the package after a failed
        // install is from that install, so nothing may be there now: uvenv
        // leaves a broken environment there when making it fails.
        let venv = uvenv_venv_dir(&package.name);
        if std::fs::symlink_metadata(&venv).is_ok() {
            let error = format!(
                "uvenv already has {}, outside Tileception; remove it with uvenv uninstall {} first",
                venv.display(),
                package.name
            );
            return Err(match aside.map_or(Ok(()), Aside::restore) {
                Ok(()) => error,
                Err(stuck) => format!("{error}; the installed version could not be put back: {stuck}"),
            });
        }
        self.step(format!("Installing {} at {} with uvenv", package.name, short(&lookup.commit)));
        let args = install_args(source, lookup);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let command = uvenv_bin_dir().join(&package.script);
        let manifest = manifest_path(&package.plugin_id);
        let had_manifest = manifest.exists();
        let wrote_before = before.as_ref().is_some_and(|installed| installed.wrote_manifest);
        // Reserve recovery state before setup can leave artifacts behind. An
        // incomplete install has no commit, so an update will retry it.
        record.plugins.insert(
            package.plugin_id.clone(),
            Installed {
                source: source.to_string(),
                commit: None,
                package: package.name.clone(),
                tool: Tool::Uvenv,
                command: command.clone(),
                wrote_manifest: wrote_before || !had_manifest,
                release: None,
                sha256: None,
            },
        );
        record.save()?;
        let mut install = self.command(&uvenv, &args);
        // uv fetches the repository with git, the same way it was looked up.
        self.quiet_git(&mut install, lookup.transport);
        let placed = self.check("uvenv install", install, PACKAGE_TIMEOUT).and_then(|_| {
            let listing = list("uvenv list")?;
            let linked = uvenv_entry(&listing.stdout, &package.name).map(|entry| uvenv_linked_scripts(&entry));
            if linked.is_some_and(|scripts| scripts.contains(&package.script)) {
                return Ok(());
            }
            // Removing the plugin would run the other program's `uninstall`.
            let _ = run(self.command(&uvenv, &Tool::Uvenv.uninstall_args(&package.name)), PACKAGE_TIMEOUT);
            Err(format!(
                "Did not install {}: another program is called {} in {}; remove or rename that one first",
                package.name,
                package.script,
                uvenv_bin_dir().display()
            ))
        });
        if let Err(error) = placed {
            if std::fs::symlink_metadata(&venv).is_ok() {
                let _ = run(self.command(&uvenv, &Tool::Uvenv.uninstall_args(&package.name)), PACKAGE_TIMEOUT);
                if let Err(error) = std::fs::remove_dir_all(&venv)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!("{}: {error}", venv.display());
                }
            }
            let restored = aside.map_or(Ok(()), Aside::restore);
            match before {
                Some(installed) => record.plugins.insert(package.plugin_id.clone(), installed),
                None => record.plugins.remove(&package.plugin_id),
            };
            record.save()?;
            return Err(match restored {
                Ok(()) => error,
                Err(stuck) => format!("{error}; the installed version could not be put back: {stuck}"),
            });
        }
        if let Some(aside) = aside {
            aside.discard();
        }
        self.finish(&mut record, lookup, before.as_ref(), &command)
    }

    /// Unpacks the release archive looked up beside the installed one, and
    /// puts it in its place once it is whole. The installed one stays aside
    /// until the plugin has set itself up, and comes back if that fails.
    fn install_release(
        &self,
        source: &Source,
        lookup: &Lookup,
        release: &Release,
    ) -> Result<(String, Option<Configure>), String> {
        let package = &lookup.package;
        let plugin_id = &package.plugin_id;
        conflict(&Record::load(), source, package)?;
        let mut record = Record::load();
        let before = record.plugins.get(plugin_id).cloned();
        let dir = release_dir(plugin_id);
        let folder = data_dir().join("set-aside").join(plugin_id);
        let ours = before.as_ref().is_some_and(|installed| installed.tool == Tool::Release);
        if Aside::recover(&folder)? {
            tracing::info!("put {plugin_id} back, which an update that never ended had set aside");
        }
        if !ours && std::fs::symlink_metadata(&dir).is_ok() {
            return Err(format!(
                "{} is in the way, and no installed plugin put it there; remove it first",
                dir.display()
            ));
        }
        let uvenv = match &before {
            Some(installed) if installed.tool == Tool::Uvenv => self.find(Tool::Uvenv)?,
            _ => PathBuf::new(),
        };
        // What is installed keeps running while this downloads and unpacks.
        self.step(format!("Downloading {} of {} {}", release.asset, package.name, release.tag));
        let staged = tileception_acp::install::stage(&release.url, &release.sha256, "", &dir)
            .map_err(|error| format!("Did not install {}: {error}", package.name))?;
        let unstage = |error: String| {
            let _ = std::fs::remove_dir_all(&staged);
            format!("Did not install {}: {error}", package.name)
        };
        let program = find_program(&staged, &package.script).map_err(unstage)?;
        let command = dir.join(program.strip_prefix(&staged).unwrap_or(&program));
        let aside = match &before {
            Some(installed) => {
                self.step(format!("Setting {} aside until its replacement is set up", installed.package));
                Some(Aside::take(folder, self.old_paths(&uvenv, plugin_id, installed)).map_err(unstage)?)
            }
            None => None,
        };
        let had_manifest = manifest_path(plugin_id).exists();
        let wrote_before = before.as_ref().is_some_and(|installed| installed.wrote_manifest);
        let swapped = swap_in(&staged, &dir, aside, || {
            // No commit until setup is through, so an update tries it again.
            record.plugins.insert(
                plugin_id.clone(),
                Installed {
                    source: source.to_string(),
                    commit: None,
                    package: package.name.clone(),
                    tool: Tool::Release,
                    command: command.clone(),
                    wrote_manifest: wrote_before || !had_manifest,
                    release: Some(release.tag.clone()),
                    sha256: Some(release.sha256.clone()),
                },
            );
            record.save()?;
            self.finish(&mut record, lookup, before.as_ref(), &command)
        });
        let (error, restored, installed) = match (swapped, before) {
            (Ok(done), _) => return Ok(done),
            (Err(Swap { error, restored }), Some(installed)) => (error, restored, installed),
            (Err(Swap { error, .. }), None) => return Err(error),
        };
        // The record goes back either way: the new version is gone, and the
        // one from before is in place or waits in set-aside to come back.
        record.plugins.insert(plugin_id.clone(), installed.clone());
        record.save()?;
        if !restored {
            return Err(error);
        }
        // The version from before is back: so is its manifest, and whatever
        // its own `install` sets up.
        let manifest = manifest_path(plugin_id);
        if std::fs::read_to_string(&manifest).is_ok_and(|text| text == minimal_manifest(&command)) {
            std::fs::write(&manifest, minimal_manifest(&installed.command))
                .map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        self.step(format!("Setting {} up again as it was", installed.package));
        let kept = match self.setup(plugin_id, &installed.command) {
            Ok(()) => "the installed version was kept".to_string(),
            Err(again) => format!("the installed version was put back, but its own install failed ({again})"),
        };
        Err(format!("{error}; {kept}"))
    }

    /// The rest of installing, once the program is in place: lets the plugin
    /// set itself up, records the commit, and asks whether it is configured.
    fn finish(
        &self,
        record: &mut Record,
        lookup: &Lookup,
        before: Option<&Installed>,
        command: &Path,
    ) -> Result<(String, Option<Configure>), String> {
        let package = &lookup.package;
        let manifest = manifest_path(&package.plugin_id);
        // A manifest Tileception wrote for where the plugin was before.
        if let Some(installed) = before
            && installed.command != command
            && std::fs::read_to_string(&manifest).is_ok_and(|text| text == minimal_manifest(&installed.command))
        {
            std::fs::write(&manifest, minimal_manifest(command))
                .map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        if !command.is_file() {
            return Err(format!("{} did not install {}", package.name, command.display()));
        }
        self.step(format!("Running {} install, which may take a while", package.script));
        self.setup(&package.plugin_id, command)?;
        record.plugins.get_mut(&package.plugin_id).expect("installation was recorded").commit =
            Some(lookup.commit.clone());
        record.save()?;
        self.step(format!("Asking {} whether it is configured", package.script));
        let status = self.status(command);
        let configure =
            (!status.configured).then(|| Configure { plugin_id: package.plugin_id.clone(), setup: status.setup });
        let version = if package.version.is_empty() { String::new() } else { format!(" {}", package.version) };
        let installed = format!("Installed {}{version} at {}", package.name, short(&lookup.commit));
        let body = match &configure {
            Some(Configure { setup: Some(_), .. }) => format!("{installed}; set it up in the launcher"),
            Some(_) => format!("{installed}; set it up in the terminal"),
            None => installed,
        };
        Ok((body, configure))
    }

    /// `install --non-interactive`, then a manifest if the plugin wrote none.
    fn setup(&self, plugin_id: &str, command: &Path) -> Result<(), String> {
        let output = run(self.command(command, &["install", "--non-interactive"]), INSTALL_TIMEOUT)?;
        // argparse ends with 2 on an unknown option: an older plugin whose
        // `install` asks nothing without a terminal anyway.
        let output = if output.status.code() == Some(2) && !output.status.success() {
            run(self.command(command, &["install"]), INSTALL_TIMEOUT)?
        } else {
            output
        };
        if !output.status.success() {
            return Err(format!("{} install failed: {}", command.display(), tail(&output)));
        }
        let manifest = manifest_path(plugin_id);
        if !manifest.exists() {
            if let Some(parent) = manifest.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&manifest, minimal_manifest(command))
                .map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        Ok(())
    }

    /// What `status` says; a plugin that does not answer counts as
    /// configured, without a setup prompt.
    fn status(&self, command: &Path) -> Status {
        match run(self.command(command, &["status"]), QUICK_TIMEOUT) {
            Ok(output) => Status::read(&output.stdout),
            Err(_) => Status { configured: true, setup: None },
        }
    }

    /// How to set up an installed plugin: with the setup prompt its
    /// `status` offers, else in a terminal.
    fn configure(&self, plugin_id: &str) -> Result<Configure, String> {
        let record = Record::load();
        let installed =
            record.plugins.get(plugin_id).ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let setup = self.status(&installed.command).setup;
        Ok(Configure { plugin_id: plugin_id.to_string(), setup })
    }

    /// Moves one plugin to the commit its source is at now: the only step
    /// that installs something other than what was installed before.
    fn update_one(&self, plugin_id: &str) -> Result<(Updated, Option<Configure>), String> {
        let record = Record::load();
        let installed =
            record.plugins.get(plugin_id).ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let source = Source::parse(&installed.source)?;
        self.step(format!("Checking {plugin_id} for updates"));
        let lookup = self.read(&source)?;
        if lookup.package.plugin_id != plugin_id {
            return Err(format!(
                "{} now installs the plugin {}; remove {plugin_id} and add it again",
                source.repository(),
                lookup.package.plugin_id
            ));
        }
        if is_current(installed, &lookup) {
            return Ok((Updated::Current, None));
        }
        let from = installed.commit.clone();
        let (_, configure) = self.install(&source, &lookup)?;
        Ok((Updated::Moved { from, onto: lookup.commit }, configure))
    }

    /// Updates each plugin in turn, whatever happens to the others.
    fn update(&self, ids: &[String]) -> (UpdateResults, Vec<Configure>) {
        let mut configure = Vec::new();
        let results = ids
            .iter()
            .map(|plugin_id| {
                let result = self.update_one(plugin_id).map(|(updated, setup)| {
                    configure.extend(setup);
                    updated
                });
                (plugin_id.clone(), result)
            })
            .collect();
        (results, configure)
    }

    fn remove(&self, plugin_id: &str) -> Result<String, String> {
        let mut record = Record::load();
        let installed = record
            .plugins
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let program = match installed.tool {
            Tool::Release => None,
            tool => Some(self.find(tool)?),
        };
        let mut notes = Vec::new();
        let manifest = manifest_path(plugin_id);
        // A plugin may remove its manifest itself. Keep unowned contents before
        // invoking it, including when uninstall or uv subsequently fails.
        let preserved = if !installed.wrote_manifest {
            match std::fs::read(&manifest) {
                Ok(contents) if contents != minimal_manifest(&installed.command).as_bytes() => Some(contents),
                Ok(_) => None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(format!("{}: {error}", manifest.display())),
            }
        } else {
            None
        };
        self.step(format!("Running {plugin_id} uninstall"));
        match run(self.command(&installed.command, &["uninstall"]), QUICK_TIMEOUT) {
            Ok(output) if output.status.success() => {}
            // argparse ends with 2 on an unknown subcommand: a plugin from
            // before `uninstall`, with nothing of its own to undo here.
            Ok(output) if output.status.code() == Some(2) => notes.push("it has no uninstall step".into()),
            Ok(output) => notes.push(format!("its uninstall failed: {}", tail(&output))),
            Err(error) => notes.push(error),
        }
        if let Some(contents) = preserved {
            if let Some(parent) = manifest.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&manifest, contents).map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        match (&program, installed.tool.program()) {
            (Some(program), Some(name)) => {
                self.check(
                    &format!("{name} uninstall {}", installed.package),
                    self.command(program, &installed.tool.uninstall_args(&installed.package)),
                    PACKAGE_TIMEOUT,
                )?;
            }
            // A release is the folder its archive was unpacked into.
            _ => {
                let dir = release_dir(plugin_id);
                if installed.command.starts_with(&dir)
                    && let Err(error) = std::fs::remove_dir_all(&dir)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(format!("{}: {error}", dir.display()));
                }
            }
        }
        if let Some(note) = remove_manifest(&installed, &manifest)? {
            notes.push(note);
        }
        record.plugins.remove(plugin_id);
        record.save()?;
        let mut body = format!("Removed {}", installed.package);
        if !notes.is_empty() {
            body += &format!("; {}", notes.join("; "));
        }
        Ok(body)
    }
}

/// What a plugin's `status` says about its setup.
#[derive(Debug, PartialEq)]
struct Status {
    /// `{"configured": false}` asks to be set up; a plugin without it
    /// counts as configured.
    configured: bool,
    /// `"setup"`: a launcher prompt that sets it up, used instead of a
    /// terminal running `configure`. An invalid one is ignored.
    setup: Option<Prompt>,
}

impl Status {
    fn read(stdout: &[u8]) -> Self {
        let Ok(status) = serde_json::from_slice::<serde_json::Value>(stdout) else {
            return Status { configured: true, setup: None };
        };
        let configured = status.get("configured").and_then(serde_json::Value::as_bool).unwrap_or(true);
        let setup = status.get("setup").and_then(|setup| match Prompt::deserialize(setup) {
            Ok(prompt) => Some(prompt),
            Err(error) => {
                tracing::warn!("ignoring a plugin's setup prompt: {error}");
                None
            }
        });
        Status { configured, setup }
    }
}

/// What updating one plugin did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Updated {
    /// Installed a newer commit.
    Moved { from: Option<String>, onto: String },
    /// Its source is still at the commit installed.
    Current,
}

type UpdateResults = Vec<(String, Result<Updated, String>)>;

/// The notification for updating plugins: a title, then which failed, which
/// were updated and which were up to date, then a line for each failure and
/// each update. Ok when every one succeeded.
fn update_summary(results: &[(String, Result<Updated, String>)]) -> (String, Result<String, String>) {
    let names = |keep: fn(&Result<Updated, String>) -> bool| -> String {
        let names: Vec<&str> =
            results.iter().filter(|(_, result)| keep(result)).map(|(plugin_id, _)| plugin_id.as_str()).collect();
        names.join(", ")
    };
    let failed = names(Result::is_err);
    let moved = names(|result| matches!(result, Ok(Updated::Moved { .. })));
    let current = names(|result| matches!(result, Ok(Updated::Current)));
    let mut lines = Vec::new();
    for (label, names) in [("Failed", &failed), ("Updated", &moved), ("Up to date", &current)] {
        if !names.is_empty() {
            lines.push(format!("{label}: {names}"));
        }
    }
    for (plugin_id, result) in results {
        match result {
            Err(error) => lines.push(format!("{plugin_id}: {error}")),
            Ok(Updated::Moved { from, onto }) => {
                let from = from.as_deref().map_or_else(String::new, |from| format!("{} ", short(from)));
                lines.push(format!("{plugin_id}: {from}\u{2192} {}", short(onto)));
            }
            Ok(Updated::Current) => {}
        }
    }
    let body = lines.join("\n");
    let failures = results.iter().filter(|(_, result)| result.is_err()).count();
    match failures {
        0 => ("Plugins".into(), Ok(body)),
        _ if results.len() == 1 => (format!("Could not update {}", results[0].0), Err(body)),
        _ => (format!("Could not update {failures} of {} plugins", results.len()), Err(body)),
    }
}

impl Tileception {
    pub fn init_installer(&mut self) {
        let (sender, receiver) = channel::channel();
        self.installer.sender = Some(sender);
        self.installer.record = Record::load();
        recover_set_aside(&self.installer.record);
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(report) = event {
                    state.installer_report(report);
                }
            })
            .expect("plugin installer source");
    }

    /// Starts a job unless one runs already.
    fn start_job(&mut self, busy: String, job: Job) -> Result<(), String> {
        if let Some(running) = &self.installer.busy {
            return Err(format!("Wait for this first: {running}"));
        }
        let sender = self.installer.sender.clone().ok_or("The plugin installer is not running")?;
        let environment = self
            .command("", &[])
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect();
        let runner = Runner { environment, sender: sender.clone() };
        if !matches!(job, Job::Look(_) | Job::Configure(_)) {
            self.installer.busy = Some(busy.clone());
        }
        std::thread::Builder::new()
            .name("plugin installer".into())
            .spawn(move || {
                let report = match job {
                    Job::Look(source) => {
                        let result = Box::new(runner.look(&source));
                        Report::Looked { source, result }
                    }
                    Job::Configure(plugin_id) => Report::Configure(runner.configure(&plugin_id)),
                    Job::Install(source, lookup) => {
                        let (result, configure) = match runner.install(&source, &lookup) {
                            Ok((body, configure)) => (Ok(body), configure.into_iter().collect()),
                            Err(error) => (Err(error), Vec::new()),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Add(source) => {
                        let installed = runner.look(&source).and_then(|lookup| runner.install(&source, &lookup));
                        let (result, configure) = match installed {
                            Ok((body, configure)) => (Ok(body), configure.into_iter().collect()),
                            Err(error) => (Err(error), Vec::new()),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Update(ids) => {
                        let (results, configure) = runner.update(&ids);
                        for (plugin_id, result) in &results {
                            match result {
                                Ok(updated) => tracing::info!("updating {plugin_id}: {updated:?}"),
                                Err(error) => tracing::warn!("updating {plugin_id}: {error}"),
                            }
                        }
                        let (title, result) = update_summary(&results);
                        Report::Done { title, result, configure }
                    }
                    Job::Remove(plugin_id) => {
                        Report::Done { title: busy, result: runner.remove(&plugin_id), configure: Vec::new() }
                    }
                };
                let _ = sender.send(report);
            })
            .map_err(|error| {
                self.installer.busy = None;
                error.to_string()
            })?;
        Ok(())
    }

    /// Reads a repository's `pyproject.toml`, to show before installing.
    pub(crate) fn plugin_look_up(&mut self, source: Source) -> Result<(), String> {
        if self.installer.looking.as_ref() == Some(&source) {
            return Ok(());
        }
        // Only once it runs: a lookup refused now must not look started,
        // or Enter would wait for it for good.
        self.start_job(String::new(), Job::Look(source.clone()))?;
        self.installer.looking = Some(source);
        Ok(())
    }

    /// Installs what [`Self::plugin_look_up`] found.
    pub(crate) fn plugin_install(&mut self, source: Source, lookup: Lookup) -> Result<String, String> {
        let busy = format!("Installing {} at {}", lookup.package.name, short(&lookup.commit));
        self.start_job(busy.clone(), Job::Install(source, Box::new(lookup)))?;
        self.notify_locally("Plugins", &format!("{busy}\u{2026}"), tileception_notify::Urgency::Low);
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// `tileception msg plugins add`: looks the repository up and installs
    /// it in one go, as asking for it on the command line is the consent.
    pub(crate) fn plugin_add(&mut self, text: &str) -> Result<String, String> {
        let source = Source::parse(text)?;
        let busy = format!("Installing {source}");
        self.start_job(busy.clone(), Job::Add(source))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// Updates the named plugins, or every installed one.
    pub(crate) fn plugin_update(&mut self, plugin_id: Option<&str>) -> Result<String, String> {
        let record = Record::load();
        let ids: Vec<String> = match plugin_id {
            Some(plugin_id) if record.plugins.contains_key(plugin_id) => vec![plugin_id.to_string()],
            Some(plugin_id) => return Err(format!("{plugin_id} was not installed from a repository")),
            None if record.plugins.is_empty() => return Err("No plugins were installed from a repository".into()),
            None => record.plugins.keys().cloned().collect(),
        };
        let busy = format!("Updating {}", ids.join(", "));
        self.start_job(busy.clone(), Job::Update(ids))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    pub(crate) fn plugin_remove(&mut self, plugin_id: &str) -> Result<String, String> {
        if !Record::load().plugins.contains_key(plugin_id) {
            return Err(format!("{plugin_id} was not installed from a repository"));
        }
        let busy = format!("Removing {plugin_id}");
        self.start_job(busy.clone(), Job::Remove(plugin_id.to_string()))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// Sets a plugin up again: asks its `status` for a setup prompt in the
    /// background, then shows that or runs `configure` in a terminal.
    pub(crate) fn plugin_configure(&mut self, plugin_id: &str) -> Result<String, String> {
        if !Record::load().plugins.contains_key(plugin_id) {
            return Err(format!("{plugin_id} was not installed from a repository"));
        }
        self.start_job(String::new(), Job::Configure(plugin_id.to_string()))?;
        Ok(format!("Configuring {plugin_id}\n"))
    }

    /// Shows a plugin's setup prompt in the launcher, or runs its
    /// `configure` in a terminal when it has none or the launcher cannot
    /// show it now.
    fn set_up(&mut self, configure: Configure) {
        let Configure { plugin_id, setup } = configure;
        if let Some(prompt) = setup
            && self.open_setup_prompt(plugin_id.clone(), prompt)
        {
            return;
        }
        if let Err(error) = self.configure_in_terminal(&plugin_id) {
            self.notify_locally("Plugins", &error, tileception_notify::Urgency::Normal);
        }
    }

    /// Runs a plugin's `configure` in a terminal, then reloads plugins.
    fn configure_in_terminal(&mut self, plugin_id: &str) -> Result<String, String> {
        let record = Record::load();
        let installed =
            record.plugins.get(plugin_id).ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let tileception =
            std::env::current_exe().map_or_else(|_| "tileception".into(), |path| path.display().to_string());
        let line = format!(
            "{command} configure; status=$?; {tileception} msg plugins reload >/dev/null; echo; \
             if [ $status = 0 ]; then echo 'Done. Press Enter to close.'; \
             else echo \"Setup ended with exit status $status. Press Enter to close.\"; fi; read -r _",
            command = crate::state::shell_word(&installed.command.display().to_string()),
            tileception = crate::state::shell_word(&tileception),
        );
        self.run_in_terminal(&line)?;
        Ok(format!("Configuring {plugin_id} in a terminal\n"))
    }

    fn installer_report(&mut self, report: Report) {
        match report {
            Report::Looked { source, result } => {
                if self.installer.looking.as_ref() == Some(&source) {
                    self.installer.looking = None;
                }
                self.installer.looked = Some((source, *result));
            }
            // A step of a job that already ended is old news.
            Report::Step(step) => {
                if self.installer.busy.is_some() {
                    self.installer.busy = Some(step);
                }
            }
            Report::Done { title, result, configure } => {
                self.installer.busy = None;
                self.installer.looked = None;
                self.installer.record = Record::load();
                let _ = self.reload_plugins();
                match result {
                    Ok(body) => self.notify_locally("Plugins", &body, tileception_notify::Urgency::Normal),
                    Err(error) => {
                        tracing::warn!("{title}: {error}");
                        self.notify_locally(&title, &error, tileception_notify::Urgency::Normal)
                    }
                }
                for configure in configure {
                    self.set_up(configure);
                }
            }
            Report::Configure(Ok(configure)) => self.set_up(configure),
            Report::Configure(Err(error)) => {
                self.notify_locally("Plugins", &error, tileception_notify::Urgency::Normal)
            }
        }
        self.installer_changed();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        let odoo = Source { owner: "robinvandernoord".into(), repo: "tileception-odoo".into(), rev: None };
        for text in [
            "robinvandernoord/tileception-odoo",
            " robinvandernoord/tileception-odoo ",
            "https://github.com/robinvandernoord/tileception-odoo",
            "https://github.com/robinvandernoord/tileception-odoo/",
            "github.com/robinvandernoord/tileception-odoo.git",
            "git+https://github.com/robinvandernoord/tileception-odoo",
        ] {
            assert_eq!(Source::parse(text), Ok(odoo.clone()), "{text}");
        }
        let tagged = Source::parse("robinvandernoord/tileception-2fas@v0.2.0").expect("a tagged source");
        assert_eq!(tagged.rev.as_deref(), Some("v0.2.0"));
        assert_eq!(tagged.to_string(), "robinvandernoord/tileception-2fas@v0.2.0");
        let commit = "fb54a60c53c20be83553d78bedd8093f44332e0d";
        assert_eq!(
            tagged.git_url(commit, Transport::Https),
            "git+https://github.com/robinvandernoord/tileception-2fas@fb54a60c53c20be83553d78bedd8093f44332e0d"
        );
        assert_eq!(
            tagged.raw_url(commit, "pyproject.toml"),
            "https://raw.githubusercontent.com/robinvandernoord/tileception-2fas/fb54a60c53c20be83553d78bedd8093f44332e0d/pyproject.toml"
        );
        assert!(tagged.same_repository(&Source::parse("RobinVanDerNoord/Tileception-2fas").expect("a source")));
        assert!(!tagged.same_repository(&Source::parse("someone/tileception-2fas").expect("a source")));
        for text in [
            "",
            "odoo",
            "a/b/c",
            "../b",
            "a/.git",
            "a b/c",
            "a/b@",
            "a/b@../../x",
            "a/b@-x",
            "https://gitlab.com/a/b",
            "a/b@x y",
        ] {
            assert!(Source::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn packages() {
        let plain = package(
            r#"
[project]
name = "tileception-odoo"
version = "0.1.0"
description = "Odoo search"
requires-python = ">=3.14"
[project.scripts]
tileception-odoo = "tileception_odoo.cli:main"
"#,
        )
        .expect("a plain pyproject");
        assert_eq!(plain.plugin_id, "odoo");
        assert_eq!(plain.script, "tileception-odoo");
        assert_eq!(plain.python.as_deref(), Some(">=3.14"));
        let source = Source::parse("robinvandernoord/tileception-odoo").expect("a plain source");
        let commit = "0123456789abcdef0123456789abcdef01234567".to_string();
        let lookup = Lookup { package: plain, commit: commit.clone(), transport: Transport::Https, release: None };
        // What installs is the commit that was looked up, not whatever the
        // default branch is at by then.
        assert_eq!(
            install_args(&source, &lookup),
            [
                "install",
                "--python",
                ">=3.14",
                "tileception-odoo @ git+https://github.com/robinvandernoord/tileception-odoo@0123456789abcdef0123456789abcdef01234567"
            ]
        );

        let declared = package(
            r#"
[project]
name = "tileception-2fas"
[project.scripts]
tileception-2fas = "a:main"
other = "b:main"
[tool.tileception]
id = "totp"
script = "tileception-2fas"
extras = ["security-key"]
needs = ["the 2fas CLI"]
"#,
        )
        .expect("a declared pyproject");
        assert_eq!((declared.plugin_id.as_str(), declared.needs.len()), ("totp", 1));
        assert_eq!(
            requirement(
                &source,
                &Lookup { package: declared, commit: commit.clone(), transport: Transport::Https, release: None }
            ),
            "tileception-2fas[security-key] @ git+https://github.com/robinvandernoord/tileception-odoo@0123456789abcdef0123456789abcdef01234567"
        );

        // Two scripts and none named: which one serves is unknown.
        assert!(package("[project]\nname = \"x\"\n[project.scripts]\na = \"a:m\"\nb = \"b:m\"\n").is_err());
        assert!(package("[tool.poetry]\nname = \"x\"\n").is_err());
        assert!(
            package("[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nid = \"../x\"\n")
                .is_err()
        );
        assert!(
            package("[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nid = \"plugins\"\n")
                .is_err()
        );
        assert!(
            package(
                "[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nextras = [\"a]; rm\"]\n"
            )
            .is_err()
        );
    }

    #[test]
    fn status() {
        let configured = |stdout: &[u8]| Status::read(stdout).configured;
        assert!(!configured(br#"{"configured": false, "detail": "no API key"}"#));
        assert!(configured(br#"{"configured": true}"#));
        // Older plugins print other status, or none.
        assert!(configured(br#"{"synced": 3}"#));
        assert!(configured(b"all fine"));
        assert_eq!(Status::read(b"all fine").setup, None);
        let status = Status::read(
            br#"{"configured": false, "setup": {"title": "Set up Odoo", "steps": [{"id": "connect",
                "kind": "fields", "tool": "setup", "timeout": 30,
                "fields": [{"name": "web_url", "label": "Web URL", "value": "https://example.com"},
                           {"name": "api_key", "label": "API key", "secret": true}]}]}}"#,
        );
        assert!(!status.configured);
        let setup = status.setup.expect("a setup prompt");
        assert_eq!(setup.title, "Set up Odoo");
        let crate::plugins::StepKind::Fields { fields, .. } = &setup.steps[0].kind else { panic!("fields") };
        assert_eq!(fields[0].value, "https://example.com");
        // A bad prompt still asks for setup, in a terminal.
        let status = Status::read(br#"{"configured": false, "setup": {"title": "Broken", "steps": []}}"#);
        assert_eq!(status, Status { configured: false, setup: None });
    }

    #[test]
    fn runs_with_a_deadline() {
        let output = {
            let mut command = Command::new("sh");
            command.args(["-c", "echo out; echo err >&2; exit 3"]);
            run(command, QUICK_TIMEOUT)
        }
        .expect("sh runs");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(tail(&output), "err");
        // uvenv colours its errors and rewrites a progress line in place,
        // even without a terminal.
        let mut command = Command::new("printf");
        command.arg(
            "\\r\\342\\243\\267 installing x \\r\\033[2KScript x already exists in \\033[32m~/.local/bin\\033[39m.\\n",
        );
        let output = run(command, QUICK_TIMEOUT).expect("printf runs");
        assert_eq!(tail(&output), "Script x already exists in ~/.local/bin.");
        let mut sleep = Command::new("sleep");
        sleep.arg("5");
        assert!(run(sleep, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_program_that_leaves_something_running_still_returns() {
        // An `install` that starts a background process, which keeps its
        // output open after the plugin's own process has ended.
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & echo started"]);
        let started = Instant::now();
        let output = run(command, Duration::from_secs(2)).expect("sh runs");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
    }

    #[test]
    fn lookups_do_not_share_a_file() {
        assert_ne!(lookup_file(), lookup_file());
    }

    fn installed(source: &str, package: &str) -> Installed {
        Installed {
            source: source.into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            package: package.into(),
            tool: Tool::Uvenv,
            command: format!("/home/me/.local/bin/{package}").into(),
            wrote_manifest: true,
            release: None,
            sha256: None,
        }
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            browser_download_url: format!("https://github.com/me/tileception-dice/releases/download/v1.2.0/{name}"),
            digest: None,
        }
    }

    #[test]
    fn release_archives_match_the_machine() {
        let assets: Vec<Asset> = [
            "tileception-dice-v1.2.0-aarch64-unknown-linux-gnu.tar.gz",
            "tileception-dice-v1.2.0-aarch64-unknown-linux-musl.tar.gz",
            "tileception-dice-v1.2.0-x86_64-unknown-linux-musl.tar.gz",
            "tileception-dice-v1.2.0-x86_64-unknown-linux-gnu.tar.gz",
            "tileception-dice-v1.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "tileception-dice-v1.2.0-x86_64-apple-darwin.tar.gz",
            "tileception-dice-v1.2.0-x86_64-pc-windows-msvc.zip",
            "tileception-dice-v1.2.0-aarch64-linux-android.tar.gz",
            "checksums.txt",
        ]
        .map(asset)
        .to_vec();
        let picked =
            |assets: &[Asset], architecture: &str| pick_asset(assets, architecture).map(|asset| asset.name.clone());
        assert_eq!(
            picked(&assets, "x86_64").as_deref(),
            Some("tileception-dice-v1.2.0-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            picked(&assets, "aarch64").as_deref(),
            Some("tileception-dice-v1.2.0-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(picked(&assets, "riscv64"), None);
        // The other spellings, as GoReleaser and others write them.
        let goreleaser: Vec<Asset> = [
            "dice_1.2.0_Linux_amd64.tar.gz",
            "dice_1.2.0_Linux_arm64.tar.gz",
            "dice_1.2.0_Linux_armv7.tar.gz",
            "dice_1.2.0_Darwin_arm64.tar.gz",
        ]
        .map(asset)
        .to_vec();
        assert_eq!(picked(&goreleaser, "x86_64").as_deref(), Some("dice_1.2.0_Linux_amd64.tar.gz"));
        assert_eq!(picked(&goreleaser, "aarch64").as_deref(), Some("dice_1.2.0_Linux_arm64.tar.gz"));
        assert_eq!(picked(&goreleaser, "arm"), None);
        // Without a glibc build, one that does not say comes before musl.
        let plain = ["dice-linux-x64-musl.zip", "dice-linux-x64.zip"].map(asset).to_vec();
        assert_eq!(picked(&plain, "x86_64").as_deref(), Some("dice-linux-x64.zip"));
        assert_eq!(picked(&plain[..1], "x86_64").as_deref(), Some("dice-linux-x64-musl.zip"));
        // A build for newer processors or for debugging does not win a tie,
        // whatever the order of the names.
        let variants = [
            "dice-linux-amd64-debug.tar.gz",
            "dice-linux-amd64-v3.tar.gz",
            "dice-linux-amd64.tar.gz",
            "dice-linux-amd64.tar.gz.zip",
        ]
        .map(asset)
        .to_vec();
        assert_eq!(picked(&variants, "x86_64").as_deref(), Some("dice-linux-amd64.tar.gz"));
        assert_eq!(picked(&variants[..2], "x86_64").as_deref(), Some("dice-linux-amd64-v3.tar.gz"));
        // A bare program or an installer is not an archive.
        let bare = ["dice-linux-x86_64", "dice-linux-x86_64.deb"].map(asset).to_vec();
        assert_eq!(picked(&bare, "x86_64"), None);
        assert!(names_part("tool_Linux_amd64", "linux"));
        assert!(!names_part("tool-arm64", "arm"));
        // A 32-bit machine does not take the 64-bit archive.
        assert_eq!(picked(&assets, "x86"), None);
        assert_eq!(
            picked(&[asset("dice-i686-unknown-linux-gnu.tar.gz")], "x86").as_deref(),
            Some("dice-i686-unknown-linux-gnu.tar.gz")
        );
    }

    #[test]
    fn release_checksums() {
        let hash = "5d41402abc4b2a76b9719d911017c592ae2c8a4f2b6d3c1e9a7b5c3d1e0f9a8b";
        let archive = "dice-x86_64-unknown-linux-gnu.tar.gz";
        // GitHub's own digest comes first.
        let digested = Asset { digest: Some(format!("sha256:{}", hash.to_uppercase())), ..asset(archive) };
        let assets = vec![digested.clone(), asset("checksums.txt")];
        assert_eq!(pick_checksum(&assets, &digested), Some(Checksum::Known(hash.into())));
        // Then a file about the archive alone, then a list.
        let plain = asset(archive);
        let own = asset("dice-x86_64-unknown-linux-gnu.tar.gz.sha256");
        let list = asset("dice_1.2.0_checksums.txt");
        assert_eq!(
            pick_checksum(&[plain.clone(), list.clone(), own.clone()], &plain),
            Some(Checksum::Listed { url: own.browser_download_url.clone(), own: true })
        );
        assert_eq!(
            pick_checksum(&[plain.clone(), asset("checksums.txt.sig"), list.clone()], &plain),
            Some(Checksum::Listed { url: list.browser_download_url.clone(), own: false })
        );
        assert_eq!(
            pick_checksum(&[plain.clone(), asset("SHA256SUMS")], &plain),
            Some(Checksum::Listed { url: asset("SHA256SUMS").browser_download_url, own: false })
        );
        // A list of another hash does not shadow the SHA-256 one, in
        // either order, and is not read as SHA-256 on its own.
        let (sha512, sha256, unnamed) =
            (asset("dice_sha512-checksums.txt"), asset("dice_sha256-checksums.txt"), asset("checksums.txt"));
        for assets in
            [[sha512.clone(), unnamed.clone(), sha256.clone()], [sha256.clone(), sha512.clone(), unnamed.clone()]]
        {
            assert_eq!(
                pick_checksum(&assets, &plain),
                Some(Checksum::Listed { url: sha256.browser_download_url.clone(), own: false })
            );
        }
        assert_eq!(
            pick_checksum(&[sha512.clone(), unnamed.clone()], &plain),
            Some(Checksum::Listed { url: unnamed.browser_download_url.clone(), own: false })
        );
        assert_eq!(pick_checksum(&[sha512, asset("checksums.md5.txt")], &plain), None);
        // Another kind of digest, or none, is no checksum.
        let md5 = Asset { digest: Some("md5:abc".into()), ..asset(archive) };
        assert_eq!(pick_checksum(std::slice::from_ref(&md5), &md5), None);
        assert_eq!(pick_checksum(&[plain.clone(), asset("README.md")], &plain), None);

        let other = "a".repeat(64);
        let text = format!("{other}  dice-aarch64-unknown-linux-gnu.tar.gz\n{hash} *./{archive}\n");
        assert_eq!(checksum_in(&text, archive, false).as_deref(), Some(hash));
        assert_eq!(checksum_in(&text, "dice.zip", false), None);
        assert_eq!(checksum_in(&format!("{hash}\n"), archive, true).as_deref(), Some(hash));
        // A lone hash in a list is not about this archive.
        assert_eq!(checksum_in(&format!("{hash}\n"), archive, false), None);
        assert_eq!(checksum_in("not-a-hash  dice.zip\n", "dice.zip", true), None);
        assert_eq!(sha256_hex(&hash[1..]), None);
    }

    #[test]
    fn releases_are_chosen_from_githubs_listing() {
        let source = Source::parse("me/tileception-dice").unwrap();
        let hash = "b".repeat(64);
        let listing = format!(
            r#"{{"tag_name": "v1.2.0", "name": "Dice 1.2", "assets": [
                {{"name": "dice-aarch64-unknown-linux-gnu.tar.gz", "digest": "sha256:{hash}",
                  "browser_download_url": "https://github.com/me/tileception-dice/releases/download/v1.2.0/dice-aarch64-unknown-linux-gnu.tar.gz"}},
                {{"name": "dice-x86_64-unknown-linux-gnu.tar.gz", "digest": null,
                  "browser_download_url": "https://github.com/me/tileception-dice/releases/download/v1.2.0/dice-x86_64-unknown-linux-gnu.tar.gz"}}
            ]}}"#
        );
        let (tag, archive, checksum) = choose_release(&source, &listing, "aarch64").unwrap();
        assert_eq!((tag.as_str(), archive.name.as_str()), ("v1.2.0", "dice-aarch64-unknown-linux-gnu.tar.gz"));
        assert_eq!(checksum, Checksum::Known(hash));
        // An archive nothing vouches for is refused, and so is a machine
        // the release was not built for.
        let error = choose_release(&source, &listing, "x86_64").unwrap_err();
        assert!(error.contains("publishes no SHA-256 checksum for dice-x86_64-unknown-linux-gnu.tar.gz"), "{error}");
        let error = choose_release(&source, &listing, "riscv64").unwrap_err();
        assert!(error.contains("no archive for Linux on riscv64"), "{error}");
        assert!(error.contains("dice-aarch64-unknown-linux-gnu.tar.gz, dice-x86_64"), "{error}");
        let error = choose_release(&source, r#"{"tag_name": "v1"}"#, "x86_64").unwrap_err();
        assert!(error.contains("it has no files"), "{error}");
        assert!(choose_release(&source, r#"{"tag_name": "v1 --upload-pack=x", "assets": []}"#, "x86_64").is_err());
        assert!(choose_release(&source, r#"{"message": "Not Found"}"#, "x86_64").is_err());

        assert_eq!(
            release_url("https://api.github.com", &source),
            "https://api.github.com/repos/me/tileception-dice/releases/latest"
        );
        assert_eq!(
            release_url("https://api.github.com", &Source::parse("me/tileception-dice@v1.2.0").unwrap()),
            "https://api.github.com/repos/me/tileception-dice/releases/tags/v1.2.0"
        );
        // Only the repository's own release files are downloaded.
        let downloads = "https://github.com";
        assert!(release_file(
            downloads,
            &source,
            "https://github.com/Me/Tileception-Dice/releases/download/v1.2.0/dice.tar.gz"
        ));
        for url in [
            "https://github.com/other/tileception-dice/releases/download/v1.2.0/dice.tar.gz",
            "https://example.com/me/tileception-dice/releases/download/v1.2.0/dice.tar.gz",
            "https://github.com/me/tileception-dice/releases/download/../../../other/x/dice.tar.gz",
            "http://github.com/me/tileception-dice/releases/download/v1.2.0/dice.tar.gz",
        ] {
            assert!(!release_file(downloads, &source, url), "{url}");
        }
    }

    #[test]
    fn a_repository_without_pyproject_falls_through_to_its_release() {
        // curl on GitHub's 404, and git on a commit without the file.
        assert!(no_such_file("download failed: curl: (22) The requested URL returned error: 404"));
        assert!(no_such_file("git show failed: fatal: path 'pyproject.toml' does not exist in 'FETCH_HEAD'"));
        // Anything else is a failure to read, not a missing file.
        assert!(!no_such_file("download failed: curl: (6) Could not resolve host: raw.githubusercontent.com"));
        assert!(!no_such_file("download failed: curl: (22) The requested URL returned error: 500"));

        let release = Release {
            tag: "v1.2.0".into(),
            asset: "dice-x86_64-unknown-linux-gnu.tar.gz".into(),
            url: asset("dice-x86_64-unknown-linux-gnu.tar.gz").browser_download_url,
            sha256: "c".repeat(64),
        };
        let package = release_package(&Source::parse("me/Tileception-Dice").unwrap(), &release).unwrap();
        assert_eq!((package.name.as_str(), package.script.as_str()), ("Tileception-Dice", "Tileception-Dice"));
        assert_eq!((package.plugin_id.as_str(), package.version.as_str()), ("dice", "v1.2.0"));
        assert_eq!(package.python, None);
        assert!(release_package(&Source::parse("me/plugins").unwrap(), &release).is_err());
        assert!(release_package(&Source::parse("me/dice.rs").unwrap(), &release).is_err());
        assert!(release_package(&Source::parse("me/tileception-").unwrap(), &release).is_err());
    }

    #[test]
    fn the_program_of_a_release_is_found() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tileception-release-program-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = |path: &str, mode: u32| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        file("dice-1.2.0/README.md", 0o644);
        assert!(find_program(&dir, "tileception-dice").unwrap_err().contains("no program"));
        // The only program there is, whatever it is called.
        file("dice-1.2.0/bin/dice", 0o755);
        assert_eq!(find_program(&dir, "tileception-dice"), Ok(dir.join("dice-1.2.0/bin/dice")));
        // With several, the one named after the repository.
        file("dice-1.2.0/bin/helper", 0o755);
        assert!(find_program(&dir, "tileception-dice").unwrap_err().contains("several programs"));
        file("dice-1.2.0/libexec/tileception-dice", 0o755);
        file("dice-1.2.0/tileception-dice", 0o755);
        assert_eq!(find_program(&dir, "tileception-dice"), Ok(dir.join("dice-1.2.0/tileception-dice")));
        // A link is not a file of the archive, and is not run.
        std::os::unix::fs::symlink("/bin/sh", dir.join("tileception-dice")).unwrap();
        assert_eq!(find_program(&dir, "tileception-dice"), Ok(dir.join("dice-1.2.0/tileception-dice")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_update_keeps_the_installed_release() {
        let work = std::env::temp_dir().join(format!("tileception-release-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let (dir, staged, folder) =
            (work.join("releases/dice"), work.join("releases/dice.part"), work.join("aside/dice"));
        let unpack = |into: &Path, version: &str| {
            std::fs::create_dir_all(into).unwrap();
            std::fs::write(into.join("dice"), version).unwrap();
        };
        let installed = || std::fs::read_to_string(dir.join("dice")).ok();
        let aside = || Some(Aside::take(folder.clone(), vec![dir.clone()]).unwrap());
        unpack(&dir, "1.0");

        // The plugin's own install fails: the old version is back, whole.
        unpack(&staged, "1.1");
        let failed = swap_in(&staged, &dir, aside(), || {
            // What runs meanwhile is the new version, with the old one kept.
            assert_eq!(installed().as_deref(), Some("1.1"));
            assert_eq!(std::fs::read_to_string(folder.join("0/dice")).unwrap(), "1.0");
            Err::<(), _>("dice install failed".to_string())
        });
        assert_eq!(failed, Err(Swap { error: "dice install failed".into(), restored: true }));
        assert_eq!(installed().as_deref(), Some("1.0"));
        assert!(!staged.exists() && !folder.exists());

        // It works: only now is the old version gone.
        unpack(&staged, "1.1");
        assert_eq!(swap_in(&staged, &dir, aside(), || Ok(7)), Ok(7));
        assert_eq!(installed().as_deref(), Some("1.1"));
        assert!(!staged.exists() && !folder.exists());

        // A run that ended between setting aside and placing left nothing
        // installed: it comes back, and is not deleted by the next try.
        let _left = aside().unwrap();
        assert_eq!(installed(), None);
        assert_eq!(Aside::recover(&folder), Ok(true));
        assert_eq!(installed().as_deref(), Some("1.1"));
        assert!(!folder.exists() && !work.join("aside").exists());
        // The next try to set it aside puts it back first, too, whatever
        // was set aside: a uv environment and its link as well.
        let (venv, link) = (work.join("venvs/dice"), work.join("bin/dice"));
        unpack(&venv, "uv");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(venv.join("dice"), &link).unwrap();
        let _left = Aside::take(folder.clone(), vec![link.clone(), venv.clone()]).unwrap();
        assert!(!venv.exists() && std::fs::symlink_metadata(&link).is_err());
        let again = Aside::take(folder.clone(), vec![link.clone(), venv.clone()]).unwrap();
        again.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "uv");
        // With something in its place, what lies aside was replaced since.
        let _left = aside().unwrap();
        unpack(&dir, "1.2");
        assert_eq!(Aside::recover(&folder), Ok(false));
        assert_eq!(installed().as_deref(), Some("1.2"));
        assert!(!folder.exists());
        // A folder it cannot tell about is left alone.
        unpack(&folder.join("0"), "0.9");
        assert!(Aside::recover(&folder).unwrap_err().contains("put it back or remove it"));
        assert!(Aside::take(folder.clone(), vec![dir.clone()]).is_err());
        assert_eq!(std::fs::read_to_string(folder.join("0/dice")).unwrap(), "0.9");
        std::fs::remove_dir_all(&folder).unwrap();

        // A first install that fails stays, to be tried again.
        std::fs::remove_dir_all(&dir).unwrap();
        unpack(&staged, "1.0");
        let first = swap_in(&staged, &dir, None, || Err::<(), _>("dice install failed".to_string()));
        assert_eq!(first, Err(Swap { error: "dice install failed".into(), restored: false }));
        assert_eq!(installed().as_deref(), Some("1.0"));
        std::fs::remove_dir_all(&work).unwrap();
    }

    #[test]
    fn a_release_updates_only_when_it_changed() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let release = |tag: &str, sha256: &str| Release {
            tag: tag.into(),
            asset: "dice-x86_64-unknown-linux-gnu.tar.gz".into(),
            url: asset("dice-x86_64-unknown-linux-gnu.tar.gz").browser_download_url,
            sha256: sha256.into(),
        };
        let source = Source::parse("me/tileception-dice").unwrap();
        let lookup = |commit: &str, release: Release| Lookup {
            package: release_package(&source, &release).unwrap(),
            commit: commit.into(),
            transport: Transport::Https,
            release: Some(release),
        };
        let (old, new) = ("a".repeat(64), "b".repeat(64));
        let on_disk = Installed {
            tool: Tool::Release,
            release: Some("v1.2.0".into()),
            sha256: Some(old.clone()),
            ..installed("me/tileception-dice", "tileception-dice")
        };
        assert!(is_current(&on_disk, &lookup(commit, release("v1.2.0", &old))));
        // A newer release, the same tag moved, or its archive replaced.
        assert!(!is_current(&on_disk, &lookup("89abcdef0123456789abcdef0123456789abcdef", release("v1.3.0", &new))));
        assert!(!is_current(&on_disk, &lookup("89abcdef0123456789abcdef0123456789abcdef", release("v1.2.0", &old))));
        assert!(!is_current(&on_disk, &lookup(commit, release("v1.2.0", &new))));
        // One whose setup never finished has no commit, and is tried again.
        let unfinished = Installed { commit: None, ..on_disk.clone() };
        assert!(!is_current(&unfinished, &lookup(commit, release("v1.2.0", &old))));
        // A Python package is current at its commit, and no longer once the
        // repository has moved to releases.
        let python = installed("me/tileception-dice", "tileception-dice");
        let package = release_package(&source, &release("v1.2.0", &old)).unwrap();
        let plain = Lookup { package, commit: commit.into(), transport: Transport::Https, release: None };
        assert!(is_current(&python, &plain));
        assert!(!is_current(&python, &lookup(commit, release("v1.2.0", &old))));
        assert!(!is_current(&on_disk, &plain));

        // The record keeps the release, and older records read as before.
        let mut record = Record::default();
        record.plugins.insert("dice".into(), on_disk.clone());
        let text = toml::to_string(&record).unwrap();
        assert!(text.contains("tool = \"release\""), "{text}");
        assert!(text.contains("release = \"v1.2.0\""), "{text}");
        assert_eq!(toml::from_str::<Record>(&text).unwrap(), record);
        let text = toml::to_string(&Record { plugins: BTreeMap::from([("dice".to_string(), python)]) }).unwrap();
        assert!(!text.contains("release") && !text.contains("sha256"), "{text}");
        assert_eq!(Tool::Release.program(), None);
    }

    #[test]
    fn records_round_trip() {
        let mut record = Record::default();
        record.plugins.insert("odoo".into(), installed("robinvandernoord/tileception-odoo", "tileception-odoo"));
        let text = toml::to_string(&record).expect("the record serializes");
        assert_eq!(toml::from_str::<Record>(&text).expect("the record reads back"), record);
        // A record from before commits and manifests were kept still reads.
        let old = toml::from_str::<Record>("[plugins.odoo]\nsource = \"a/b\"\npackage = \"b\"\ncommand = \"/bin/b\"\n")
            .expect("an older record reads");
        assert_eq!((old.plugins["odoo"].commit.as_deref(), old.plugins["odoo"].wrote_manifest), (None, false));
        // It was installed with uv, so uv removes it.
        assert_eq!(old.plugins["odoo"].tool, Tool::UvTool);
        assert!(text.contains("tool = \"uvenv\""));
    }

    #[test]
    fn a_private_repository_is_installed_over_ssh() {
        let source = Source::parse("robinvandernoord/t3-update").expect("a source");
        assert_eq!(source.clone_url(Transport::Https), "https://github.com/robinvandernoord/t3-update");
        assert_eq!(source.clone_url(Transport::Ssh), "ssh://git@github.com/robinvandernoord/t3-update.git");
        let package = package("[project]\nname = \"tileception-t3-update\"\n[project.scripts]\nt3 = \"t3:main\"\n")
            .expect("a pyproject");
        let commit = "e4aea3ab10cb380899a9343d2650580dbda72c04".to_string();
        assert_eq!(
            requirement(&source, &Lookup { package, commit, transport: Transport::Ssh, release: None }),
            "tileception-t3-update @ git+ssh://git@github.com/robinvandernoord/t3-update.git@e4aea3ab10cb380899a9343d2650580dbda72c04"
        );
    }

    #[test]
    fn links_from_a_uv_receipt() {
        let receipt = r#"
[tool]
requirements = [{ name = "tileception-odoo" }]
entrypoints = [
    { name = "tileception-odoo", install-path = "/home/me/.local/bin/tileception-odoo", from = "tileception-odoo" },
]
"#;
        assert_eq!(uv_tool_links(receipt), [PathBuf::from("/home/me/.local/bin/tileception-odoo")]);
        assert!(uv_tool_links("not toml [").is_empty());
    }

    #[test]
    fn an_installed_version_set_aside_comes_back() {
        let root = scratch("aside");
        let venv = root.join("venvs/x");
        let bin = root.join("bin");
        std::fs::create_dir_all(venv.join("bin")).expect("venv");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(venv.join("bin/x"), "#!/bin/sh\n").expect("script");
        std::os::unix::fs::symlink(venv.join("bin/x"), bin.join("x")).expect("link");
        std::fs::write(bin.join("other"), "not ours").expect("other");
        assert!(links_into(&bin.join("x"), &venv));
        assert!(!links_into(&bin.join("other"), &venv));

        let paths = vec![bin.join("x"), root.join("missing"), venv.clone()];
        let aside = Aside::take(root.join("aside"), paths.clone()).expect("set aside");
        assert!(!venv.exists() && std::fs::symlink_metadata(bin.join("x")).is_err());
        aside.restore().expect("put back");
        assert_eq!(std::fs::read_to_string(bin.join("x")).expect("works again"), "#!/bin/sh\n");
        assert!(!root.join("aside").exists());

        Aside::take(root.join("aside"), paths).expect("set aside").discard();
        assert!(!venv.exists() && !root.join("aside").exists());
        assert!(bin.join("other").exists());
    }

    #[test]
    fn package_names_as_uvenv_has_them() {
        assert_eq!(normal_package("Tileception__Odoo.plugin"), "tileception-odoo-plugin");
        assert!(same_package("tileception_2fas", "Tileception-2FAS"));
        let listing = br#"[{"name": "tileception-odoo", "scripts": {"tileception-odoo": true, "other": false}}]"#;
        let entry = uvenv_entry(listing, "Tileception_Odoo").expect("uvenv has it");
        // uvenv installs it but does not link a script whose name another
        // program has.
        assert_eq!(uvenv_linked_scripts(&entry), ["tileception-odoo"]);
        assert!(uvenv_entry(listing, "tileception-2fas").is_none());
        assert!(uvenv_entry(b"[]", "tileception-odoo").is_none());
        assert!(uvenv_entry(b"not json", "tileception-odoo").is_none());
    }

    #[test]
    fn commits_from_ls_remote() {
        let head = "fb54a60c53c20be83553d78bedd8093f44332e0d\tHEAD\n";
        assert_eq!(pick_commit(head, None).as_deref(), Some("fb54a60c53c20be83553d78bedd8093f44332e0d"));
        assert_eq!(ls_remote_patterns(None), ["HEAD"]);
        assert_eq!(ls_remote_patterns(Some("v1")), ["refs/tags/v1", "refs/tags/v1^{}", "refs/heads/v1"]);
        // An annotated tag: the commit it points at, not the tag object; and a
        // tag before a branch of the same name.
        let listing = "\
1111111111111111111111111111111111111111\trefs/heads/v1
2222222222222222222222222222222222222222\trefs/tags/v1
3333333333333333333333333333333333333333\trefs/tags/v1^{}
";
        assert_eq!(pick_commit(listing, Some("v1")).as_deref(), Some("3333333333333333333333333333333333333333"));
        let lightweight = "2222222222222222222222222222222222222222\trefs/tags/v1\n";
        assert_eq!(pick_commit(lightweight, Some("v1")).as_deref(), Some("2222222222222222222222222222222222222222"));
        let branch = "1111111111111111111111111111111111111111\trefs/heads/main\n";
        assert_eq!(pick_commit(branch, Some("main")).as_deref(), Some("1111111111111111111111111111111111111111"));
        // Only exact names count, and only real hashes.
        assert_eq!(pick_commit("1111111111111111111111111111111111111111\trefs/heads/x/v1\n", Some("v1")), None);
        assert_eq!(pick_commit("nonsense\tHEAD\n", None), None);
        assert_eq!(pick_commit("", None), None);
        assert!(is_commit("0123456789abcdef0123456789ABCDEF01234567"));
        assert!(!is_commit("0123456"));
        assert_eq!(short("fb54a60c53c20be83553d78bedd8093f44332e0d"), "fb54a60");
    }

    #[test]
    fn another_repository_cannot_take_an_installed_id() {
        let mut record = Record::default();
        record.plugins.insert("2fas".into(), installed("robinvandernoord/tileception-2fas", "tileception-2fas"));
        let pyproject = |name: &str| {
            package(&format!("[project]\nname = \"{name}\"\n[project.scripts]\n{name} = \"m:main\"\n"))
                .expect("a pyproject")
        };
        let same = pyproject("tileception-2fas");
        // Installing or updating the same repository, at any revision.
        for text in ["robinvandernoord/tileception-2fas", "RobinVanDerNoord/tileception-2fas@v0.3.0"] {
            assert_eq!(conflict(&record, &Source::parse(text).expect("a source"), &same), Ok(()), "{text}");
        }
        let fork = Source::parse("someone/tileception-2fas").expect("a source");
        assert_eq!(
            conflict(&record, &fork, &same),
            Err("plugin ID 2fas is taken by robinvandernoord/tileception-2fas; remove it first".into())
        );
        // The same package under another ID would replace its environment.
        let renamed = package(
            "[project]\nname = \"tileception_2fas\"\n[project.scripts]\nx = \"m:main\"\n[tool.tileception]\nid = \"totp\"\n",
        )
        .expect("a pyproject");
        assert!(conflict(&record, &fork, &renamed).is_err());
        // Something else entirely is fine.
        assert_eq!(conflict(&record, &fork, &pyproject("tileception-odoo")), Ok(()));
    }

    /// A directory of its own under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-plugin-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn removing_deletes_only_a_manifest_installing_made() {
        let dir = scratch("manifests");
        let manifest = dir.join("2fas.toml");
        let mut plugin = installed("robinvandernoord/tileception-2fas", "tileception-2fas");

        // Written by hand before installing: it stays, and the user is told.
        plugin.wrote_manifest = false;
        std::fs::write(&manifest, "command = \"/opt/my-2fas\"\nargs = [\"serve\", \"--verbose\"]\n").unwrap();
        let note = remove_manifest(&plugin, &manifest).unwrap();
        assert!(manifest.exists());
        assert!(note.is_some_and(|note| note.contains("kept")));

        // Made by installing: it goes.
        plugin.wrote_manifest = true;
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        assert!(!manifest.exists());

        // An older record that does not say, with the manifest Tileception
        // writes itself: that is recognised.
        plugin.wrote_manifest = false;
        std::fs::write(&manifest, minimal_manifest(&plugin.command)).unwrap();
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        assert!(!manifest.exists());

        // Already removed by the plugin's own uninstall.
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_reports_every_plugin() {
        let commit = |digit: char| std::iter::repeat_n(digit, 40).collect::<String>();
        let results = vec![
            ("2fas".to_string(), Ok(Updated::Moved { from: Some(commit('0')), onto: commit('f') })),
            ("github".to_string(), Err("uvenv install failed: no network".to_string())),
            ("odoo".to_string(), Ok(Updated::Current)),
            ("weather".to_string(), Err("weather was not installed from a repository".to_string())),
        ];
        let (title, result) = update_summary(&results);
        assert_eq!(title, "Could not update 2 of 4 plugins");
        assert_eq!(
            result,
            Err("Failed: github, weather\n\
                 Updated: 2fas\n\
                 Up to date: odoo\n\
                 2fas: 0000000 \u{2192} fffffff\n\
                 github: uvenv install failed: no network\n\
                 weather: weather was not installed from a repository"
                .into())
        );
        let (title, result) = update_summary(&results[..1]);
        assert_eq!((title.as_str(), result), ("Plugins", Ok("Updated: 2fas\n2fas: 0000000 \u{2192} fffffff".into())));
        let (title, _) = update_summary(&results[1..2]);
        assert_eq!(title, "Could not update github");
    }
}
