//! Installed and available kernels, and what can safely go. The rules follow
//! kerntop: the running kernel is never removed, meta packages are never
//! touched, installs and removals are queued into one transaction, and
//! clean-up names exact packages instead of running `autoremove`.

use std::collections::{BTreeMap, HashSet};

use crate::Runner;

const IMAGE: &str = "linux-image-";
const UNSIGNED_IMAGE: &str = "linux-image-unsigned-";
const HEADERS: &str = "linux-headers-";
/// Longest first, so `linux-modules-extra-` is not read as `linux-modules-`.
const SUPPORT: [&str; 4] = ["linux-modules-extra-", "linux-modules-", "linux-tools-", "linux-buildinfo-"];
const PATTERNS: [&str; 5] =
    ["linux-image-*", "linux-headers-*", "linux-modules-*", "linux-tools-*", "linux-buildinfo-*"];

/// One installed kernel-related package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// Installed size in kibibytes, as dpkg records it.
    pub size: u64,
}

/// Versioned image and header packages apt knows, installed or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offers {
    pub images: Vec<String>,
    pub headers: Vec<String>,
}

/// A versioned kernel image: installed, or offered by apt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    /// The ABI and flavour, as `uname -r` prints it: `6.8.0-87-generic`.
    pub identifier: String,
    /// The image package.
    pub package: String,
    pub installed: bool,
    pub running: bool,
    /// The installed version.
    pub version: Option<String>,
    /// The image with its modules and headers, in kibibytes.
    pub size: u64,
    /// Its header packages: installed ones, else the ones an install adds.
    pub headers: Vec<String>,
}

impl Kernel {
    /// The major and minor version: `6.8`.
    pub fn series(&self) -> String {
        let mut parts = self.identifier.split(['.', '-']);
        match (parts.next(), parts.next()) {
            (Some(major), Some(minor)) => format!("{major}.{minor}"),
            _ => self.identifier.clone(),
        }
    }

    /// An unsigned build, which Secure Boot refuses to start.
    pub fn unsigned(&self) -> bool {
        self.package.starts_with(UNSIGNED_IMAGE)
    }
}

/// The ABI and flavour of a versioned image package; none for meta packages
/// such as `linux-image-generic`.
pub fn identifier(package: &str) -> Option<&str> {
    let rest = package.strip_prefix(UNSIGNED_IMAGE).or_else(|| package.strip_prefix(IMAGE))?;
    versioned(rest)
}

fn versioned(rest: &str) -> Option<&str> {
    rest.starts_with(|first: char| first.is_ascii_digit()).then_some(rest)
}

/// The version part of a versioned headers package.
fn header_identifier(package: &str) -> Option<&str> {
    versioned(package.strip_prefix(HEADERS)?)
}

/// The version part of a versioned modules, tools or buildinfo package.
fn support_identifier(package: &str) -> Option<&str> {
    versioned(SUPPORT.iter().find_map(|prefix| package.strip_prefix(prefix))?)
}

/// The numbers a kernel version starts with, to order them: `6.8.0-87-generic`
/// is `[6, 8, 0, 87]`.
pub fn version_key(identifier: &str) -> Vec<u64> {
    let mut key = Vec::new();
    for part in identifier.split(['.', '-']) {
        match part.parse() {
            Ok(number) => key.push(number),
            Err(_) => break,
        }
    }
    key
}

/// The flavour after the numbers: `-generic`.
fn flavour(identifier: &str) -> &str {
    let numbers: usize =
        identifier.split(['.', '-']).take_while(|part| part.parse::<u64>().is_ok()).map(|part| part.len() + 1).sum();
    identifier.get(numbers.saturating_sub(1)..).unwrap_or("")
}

/// `dpkg-query` lines of name, status, version and size, for installed packages.
pub fn parse_installed(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?;
            let status = fields.next()?;
            let version = fields.next()?;
            let size = fields.next().and_then(|size| size.trim().parse().ok()).unwrap_or(0);
            status.starts_with("ii").then(|| Package { name: name.to_string(), version: version.to_string(), size })
        })
        .collect()
}

/// The installed kernel images, headers and support packages.
pub fn installed(runner: &dyn Runner) -> Vec<Package> {
    let format = "${Package}\t${db:Status-Abbrev}\t${Version}\t${Installed-Size}\n";
    let mut args = vec!["-W", "-f", format];
    args.extend(PATTERNS);
    runner.run("dpkg-query", &args).map(|text| parse_installed(&text)).unwrap_or_default()
}

/// Every versioned image and header package in apt's lists.
pub fn offers(runner: &dyn Runner) -> Offers {
    let names = |pattern: &str| -> Vec<String> {
        runner
            .run("apt-cache", &["search", "--names-only", pattern])
            .map(|found| found.lines().filter_map(|line| line.split_whitespace().next()).map(str::to_string).collect())
            .unwrap_or_default()
    };
    Offers { images: names("^linux-image-(unsigned-)?[0-9]"), headers: names("^linux-headers-[0-9]") }
}

/// Whether an image that is not installed is listed: every versioned image
/// with `all_variants`, else those of the running kernel's flavour, without
/// unsigned and debug builds.
fn relevant(package: &str, running: &str, all_variants: bool) -> bool {
    let Some(found) = identifier(package) else { return false };
    if all_variants {
        return true;
    }
    let wanted = flavour(running);
    !(package.starts_with(UNSIGNED_IMAGE) || found.ends_with("-dbg") || found.ends_with("-dbgsym"))
        && (wanted.is_empty() || found.ends_with(wanted))
}

fn belongs(support: &str, image: &str) -> bool {
    image.starts_with(support) || support.starts_with(image)
}

/// The kernels, newest first: every installed image, and the offered ones
/// that the view lists.
pub fn kernels(packages: &[Package], offers: &Offers, running: &str, all_variants: bool) -> Vec<Kernel> {
    let mut kernels: Vec<Kernel> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for package in packages {
        let Some(found) = identifier(&package.name) else { continue };
        seen.insert(&package.name);
        let own = |other: &&Package| {
            header_identifier(&other.name)
                .or_else(|| support_identifier(&other.name))
                .is_some_and(|support| belongs(support, found))
        };
        // Headers shared between flavours count for each: an estimate.
        let with: u64 = packages.iter().filter(own).map(|other| other.size).sum();
        let headers = packages
            .iter()
            .filter(|other| header_identifier(&other.name).is_some_and(|header| belongs(header, found)))
            .map(|other| other.name.clone())
            .collect();
        kernels.push(Kernel {
            identifier: found.to_string(),
            package: package.name.clone(),
            installed: true,
            running: found == running,
            version: Some(package.version.clone()),
            size: package.size + with,
            headers,
        });
    }
    let present: HashSet<&str> = packages.iter().map(|package| package.name.as_str()).collect();
    let on_offer: HashSet<&str> = offers.headers.iter().map(String::as_str).collect();
    for name in &offers.images {
        if !seen.insert(name) || !relevant(name, running, all_variants) {
            continue;
        }
        let Some(found) = identifier(name) else { continue };
        let header = format!("{HEADERS}{found}");
        let headers = if on_offer.contains(header.as_str()) && !present.contains(header.as_str()) {
            vec![header]
        } else {
            Vec::new()
        };
        kernels.push(Kernel {
            identifier: found.to_string(),
            package: name.clone(),
            installed: false,
            running: false,
            version: None,
            size: 0,
            headers,
        });
    }
    kernels.sort_by(|left, right| {
        version_key(&right.identifier)
            .cmp(&version_key(&left.identifier))
            .then_with(|| right.identifier.cmp(&left.identifier))
            .then_with(|| left.package.cmp(&right.package))
    });
    kernels
}

/// The installed kernel kept as the fallback: the newest that is not running.
pub fn fallback(kernels: &[Kernel]) -> Option<&Kernel> {
    kernels.iter().find(|kernel| kernel.installed && !kernel.running)
}

/// Old kernels that can go: every installed one except the running kernel
/// and the fallback.
pub fn removable(kernels: &[Kernel]) -> Vec<&Kernel> {
    kernels.iter().filter(|kernel| kernel.installed && !kernel.running).skip(1).collect()
}

/// What to do with a kernel in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    Install,
    Remove,
}

/// Whether the queue, by image package, would leave no installed kernel
/// beside the running one.
pub fn leaves_no_fallback(kernels: &[Kernel], queue: &BTreeMap<String, Queued>) -> bool {
    !kernels.iter().filter(|kernel| !kernel.running).any(|kernel| match queue.get(&kernel.package) {
        Some(Queued::Install) => true,
        Some(Queued::Remove) => false,
        None => kernel.installed,
    })
}

fn images_after<'list>(packages: &'list [Package], removed: &[&str]) -> Vec<&'list str> {
    packages
        .iter()
        .filter(|package| !removed.contains(&package.name.as_str()))
        .filter_map(|package| identifier(&package.name))
        .collect()
}

fn unused<'list>(
    packages: &'list [Package],
    images: &[&str],
    part: impl Fn(&str) -> Option<&str>,
) -> Vec<&'list Package> {
    packages
        .iter()
        .filter(|package| part(&package.name).is_some_and(|found| !images.iter().any(|image| belongs(found, image))))
        .collect()
}

/// Installed versioned headers whose kernel is no longer installed.
pub fn unused_headers(packages: &[Package]) -> Vec<&Package> {
    unused(packages, &images_after(packages, &[]), header_identifier)
}

/// Installed modules, tools and buildinfo whose kernel is no longer installed.
pub fn unused_support(packages: &[Package]) -> Vec<&Package> {
    unused(packages, &images_after(packages, &[]), support_identifier)
}

/// Both kinds of leftover.
pub fn leftovers(packages: &[Package]) -> Vec<&Package> {
    let mut all = unused_headers(packages);
    all.extend(unused_support(packages));
    all
}

/// The packages that go with these image packages: the images, and the
/// headers and modules nothing else then uses. Refuses the running kernel
/// and anything that is not an installed kernel image.
pub fn removal(packages: &[Package], running: &str, images: &[String]) -> Result<Vec<String>, String> {
    let removed: Vec<&str> = images.iter().map(String::as_str).collect();
    for name in &removed {
        let found = packages.iter().find(|package| package.name == *name).and_then(|package| identifier(&package.name));
        match found {
            Some(found) if found == running => return Err("The running kernel cannot be removed.".into()),
            Some(_) => {}
            None => return Err(format!("{name} is not an installed kernel image.")),
        }
    }
    let before = leftovers(packages);
    let after = images_after(packages, &removed);
    let mut names: Vec<String> = images.to_vec();
    let orphaned =
        unused(packages, &after, header_identifier).into_iter().chain(unused(packages, &after, support_identifier));
    names.extend(orphaned.filter(|package| !before.contains(package)).map(|package| package.name.clone()));
    Ok(names)
}

/// What installing a kernel adds: its image and the headers that match it.
pub fn install_targets(kernel: &Kernel) -> Vec<String> {
    let mut names = vec![kernel.package.clone()];
    if !kernel.installed {
        names.extend(kernel.headers.iter().cloned());
    }
    names
}

/// The size of these packages, in bytes.
pub fn size_of(packages: &[Package], names: &[String]) -> u64 {
    packages.iter().filter(|package| names.contains(&package.name)).map(|package| package.size * 1024).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, size: u64) -> Package {
        Package { name: name.into(), version: "1".into(), size }
    }

    fn system() -> Vec<Package> {
        vec![
            package("linux-image-6.14.0-35-generic", 15_000),
            package("linux-modules-6.14.0-35-generic", 500_000),
            package("linux-headers-6.14.0-35-generic", 30_000),
            package("linux-headers-6.14.0-35", 80_000),
            package("linux-image-6.14.0-33-generic", 15_000),
            package("linux-modules-6.14.0-33-generic", 500_000),
            package("linux-image-6.8.0-87-generic", 14_000),
            package("linux-modules-6.8.0-87-generic", 400_000),
            package("linux-modules-extra-6.8.0-87-generic", 150_000),
            package("linux-image-6.8.0-85-generic", 14_000),
            package("linux-headers-6.8.0-79", 70_000),
            package("linux-headers-6.8.0-79-generic", 20_000),
            package("linux-tools-6.8.0-71-generic", 9_000),
            package("linux-image-generic", 10),
            package("linux-headers-generic", 10),
        ]
    }

    fn offers() -> Offers {
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_string()).collect();
        Offers {
            images: names(&[
                "linux-image-6.14.0-36-generic",
                "linux-image-6.14.0-35-generic",
                "linux-image-6.14.0-37-lowlatency",
                "linux-image-unsigned-6.14.0-36-generic",
                "linux-image-6.8.0-31-generic",
                "linux-image-6.8.0-31-generic-dbg",
                "linux-image-6.17.0-5-generic",
            ]),
            headers: names(&["linux-headers-6.14.0-36-generic", "linux-headers-6.14.0-36", "linux-headers-6.14.0-35"]),
        }
    }

    const RUNNING: &str = "6.14.0-35-generic";

    fn identifiers(kernels: &[Kernel]) -> Vec<&str> {
        kernels.iter().map(|kernel| kernel.identifier.as_str()).collect()
    }

    #[test]
    fn dpkg_lines_keep_installed_packages() {
        let parsed = parse_installed(
            "linux-image-6.8.0-87-generic\tii \t6.8.0-87.88\t14000\n\
             linux-image-6.8.0-31-generic\trc \t6.8.0-31.31\t\n\
             linux-kernel-headers\tun \t\t\n",
        );
        assert_eq!(
            parsed,
            vec![Package { name: "linux-image-6.8.0-87-generic".into(), version: "6.8.0-87.88".into(), size: 14_000 }]
        );
    }

    #[test]
    fn meta_packages_are_not_kernels() {
        assert_eq!(identifier("linux-image-generic"), None);
        assert_eq!(identifier("linux-image-6.8.0-87-generic"), Some("6.8.0-87-generic"));
        assert_eq!(identifier("linux-image-unsigned-6.8.0-87-generic"), Some("6.8.0-87-generic"));
        assert_eq!(version_key("6.14.0-35-generic"), vec![6, 14, 0, 35]);
        assert_eq!(flavour("6.14.0-35-generic"), "-generic");
        assert_eq!(flavour("6.18.44-fc-v338"), "-fc-v338");
    }

    #[test]
    fn the_recommended_view_lists_the_running_flavour() {
        let found = kernels(&system(), &offers(), RUNNING, false);
        assert_eq!(
            identifiers(&found),
            [
                "6.17.0-5-generic",
                "6.14.0-36-generic",
                "6.14.0-35-generic",
                "6.14.0-33-generic",
                "6.8.0-87-generic",
                "6.8.0-85-generic",
                "6.8.0-31-generic",
            ]
        );
        assert!(!found[1].installed);
        assert_eq!(found[1].headers, ["linux-headers-6.14.0-36-generic"]);
        assert!(found[2].running);
        assert_eq!(found[2].size, 15_000 + 500_000 + 30_000 + 80_000);
        assert_eq!(found[2].headers, ["linux-headers-6.14.0-35-generic", "linux-headers-6.14.0-35"]);
        assert_eq!(found[4].size, 14_000 + 400_000 + 150_000);
        assert_eq!(found[2].series(), "6.14");
        assert_eq!(fallback(&found).map(|kernel| kernel.identifier.as_str()), Some("6.14.0-33-generic"));
        let old: Vec<&str> = removable(&found).iter().map(|kernel| kernel.identifier.as_str()).collect();
        assert_eq!(old, ["6.8.0-87-generic", "6.8.0-85-generic"]);
    }

    #[test]
    fn all_variants_adds_other_flavours_unsigned_and_debug_builds() {
        let found = kernels(&system(), &offers(), RUNNING, true);
        assert_eq!(found.len(), 10);
        assert!(found.iter().any(|kernel| kernel.identifier == "6.14.0-37-lowlatency"));
        assert!(found.iter().any(|kernel| kernel.identifier == "6.8.0-31-generic-dbg"));
        let unsigned: Vec<&Kernel> = found.iter().filter(|kernel| kernel.unsigned()).collect();
        assert_eq!(unsigned.len(), 1);
        assert_eq!(unsigned[0].identifier, "6.14.0-36-generic");
    }

    #[test]
    fn installing_a_kernel_takes_its_headers_along() {
        let found = kernels(&system(), &offers(), RUNNING, false);
        assert_eq!(install_targets(&found[1]), ["linux-image-6.14.0-36-generic", "linux-headers-6.14.0-36-generic"]);
        assert_eq!(install_targets(&found[0]), ["linux-image-6.17.0-5-generic"]);
    }

    #[test]
    fn leftovers_have_no_kernel() {
        let packages = system();
        let names = |list: Vec<&Package>| list.into_iter().map(|package| package.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(unused_headers(&packages)), ["linux-headers-6.8.0-79", "linux-headers-6.8.0-79-generic"]);
        assert_eq!(names(unused_support(&packages)), ["linux-tools-6.8.0-71-generic"]);
        assert_eq!(leftovers(&packages).len(), 3);
    }

    #[test]
    fn removal_takes_the_modules_along_and_spares_the_running_kernel() {
        let packages = system();
        assert!(removal(&packages, RUNNING, &["linux-image-6.14.0-35-generic".into()]).is_err());
        assert!(removal(&packages, RUNNING, &["linux-image-6.1.0-1-generic".into()]).is_err());
        assert!(removal(&packages, RUNNING, &["linux-image-generic".into()]).is_err());
        let names = removal(&packages, RUNNING, &["linux-image-6.8.0-87-generic".into()]).unwrap_or_default();
        assert_eq!(
            names,
            ["linux-image-6.8.0-87-generic", "linux-modules-6.8.0-87-generic", "linux-modules-extra-6.8.0-87-generic"]
        );
        assert_eq!(size_of(&packages, &names), (14_000 + 400_000 + 150_000) * 1024);
    }

    #[test]
    fn the_queue_is_checked_for_a_fallback() {
        let found = kernels(&system(), &offers(), RUNNING, false);
        let mut queue = BTreeMap::new();
        assert!(!leaves_no_fallback(&found, &queue));
        for name in ["linux-image-6.14.0-33-generic", "linux-image-6.8.0-87-generic", "linux-image-6.8.0-85-generic"] {
            queue.insert(name.to_string(), Queued::Remove);
        }
        assert!(leaves_no_fallback(&found, &queue));
        queue.insert("linux-image-6.14.0-36-generic".into(), Queued::Install);
        assert!(!leaves_no_fallback(&found, &queue));
    }
}
