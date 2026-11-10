//! Installed kernels and what can safely go. The rules follow kerntop: the
//! running kernel is never removed, meta packages are never touched, and
//! clean-up names exact packages instead of running `autoremove`.

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

/// A versioned kernel image: installed, or offered by apt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    /// The ABI and flavour, as `uname -r` prints it: `6.8.0-87-generic`.
    pub identifier: String,
    /// The image package.
    pub package: String,
    pub installed: bool,
    pub running: bool,
    /// The image with its modules and headers, in kibibytes.
    pub size: u64,
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
}

/// The ABI and flavour of a versioned image package; none for meta packages
/// such as `linux-image-generic`.
pub fn identifier(package: &str) -> Option<&str> {
    let rest = package.strip_prefix(UNSIGNED_IMAGE).or_else(|| package.strip_prefix(IMAGE))?;
    rest.starts_with(|first: char| first.is_ascii_digit()).then_some(rest)
}

/// The version part of a headers or support package, when it has one.
fn support_identifier(package: &str) -> Option<&str> {
    let rest =
        SUPPORT.iter().find_map(|prefix| package.strip_prefix(prefix)).or_else(|| package.strip_prefix(HEADERS))?;
    rest.starts_with(|first: char| first.is_ascii_digit()).then_some(rest)
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

/// Images apt offers that are newer than every installed one, of the running
/// kernel's flavour: the newest of each series.
pub fn offered(runner: &dyn Runner, packages: &[Package], running: &str) -> Vec<String> {
    let Ok(found) = runner.run("apt-cache", &["search", "--names-only", "^linux-image-[0-9]"]) else {
        return Vec::new();
    };
    let names = found.lines().filter_map(|line| line.split_whitespace().next());
    newer_offers(names, packages, running)
}

fn newer_offers<'list>(names: impl Iterator<Item = &'list str>, packages: &[Package], running: &str) -> Vec<String> {
    let wanted = flavour(running);
    let newest_installed = packages
        .iter()
        .filter_map(|package| identifier(&package.name))
        .map(version_key)
        .chain(std::iter::once(version_key(running)))
        .max()
        .unwrap_or_default();
    let mut newer: Vec<&str> = names
        .filter(|name| !name.starts_with(UNSIGNED_IMAGE) && !name.ends_with("-dbg"))
        .filter(|name| identifier(name).is_some_and(|found| wanted.is_empty() || flavour(found) == wanted))
        .filter(|name| identifier(name).is_some_and(|found| version_key(found) > newest_installed))
        .filter(|name| !packages.iter().any(|package| package.name == *name))
        .collect();
    newer.sort_by_key(|name| std::cmp::Reverse(identifier(name).map(version_key)));
    // Only the newest of each series: older builds of it are of no use.
    let mut series = Vec::new();
    newer.retain(|name| {
        let key: Vec<u64> = identifier(name).map(version_key).unwrap_or_default().into_iter().take(2).collect();
        let fresh = !series.contains(&key);
        series.push(key);
        fresh
    });
    newer.into_iter().map(str::to_string).collect()
}

fn belongs(support: &str, image: &str) -> bool {
    image.starts_with(support) || support.starts_with(image)
}

/// The kernels, newest first.
pub fn kernels(packages: &[Package], offered: &[String], running: &str) -> Vec<Kernel> {
    let mut kernels: Vec<Kernel> = Vec::new();
    for package in packages {
        let Some(found) = identifier(&package.name) else { continue };
        if let Some(known) = kernels.iter_mut().find(|kernel| kernel.identifier == found) {
            known.size += package.size;
            continue;
        }
        // Headers shared between flavours count for each: an estimate.
        let with = packages
            .iter()
            .filter(|other| support_identifier(&other.name).is_some_and(|support| belongs(support, found)))
            .map(|other| other.size)
            .sum::<u64>();
        kernels.push(Kernel {
            identifier: found.to_string(),
            package: package.name.clone(),
            installed: true,
            running: found == running,
            size: package.size + with,
        });
    }
    for name in offered {
        if let Some(found) = identifier(name)
            && !kernels.iter().any(|kernel| kernel.identifier == found)
        {
            kernels.push(Kernel {
                identifier: found.to_string(),
                package: name.clone(),
                installed: false,
                running: false,
                size: 0,
            });
        }
    }
    kernels.sort_by(|left, right| {
        version_key(&right.identifier).cmp(&version_key(&left.identifier)).then(right.identifier.cmp(&left.identifier))
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

fn images_after<'list>(packages: &'list [Package], removed: &[&str]) -> Vec<&'list str> {
    packages.iter().filter_map(|package| identifier(&package.name)).filter(|found| !removed.contains(found)).collect()
}

fn unused<'list>(packages: &'list [Package], images: &[&str]) -> Vec<&'list Package> {
    packages
        .iter()
        .filter(|package| {
            support_identifier(&package.name).is_some_and(|support| !images.iter().any(|image| belongs(support, image)))
        })
        .collect()
}

/// Headers, modules and tools whose kernel is no longer installed.
pub fn leftovers(packages: &[Package]) -> Vec<&Package> {
    unused(packages, &images_after(packages, &[]))
}

/// The packages to purge to remove these kernels: their images, and the
/// headers and modules nothing else then uses. Refuses the running kernel.
pub fn removal(packages: &[Package], running: &str, identifiers: &[String]) -> Result<Vec<String>, String> {
    let removed: Vec<&str> = identifiers.iter().map(String::as_str).collect();
    if removed.contains(&running) {
        return Err("The running kernel cannot be removed.".into());
    }
    for wanted in &removed {
        if !packages.iter().any(|package| identifier(&package.name) == Some(wanted)) {
            return Err(format!("Kernel {wanted} is not installed."));
        }
    }
    let before = leftovers(packages);
    let mut names: Vec<String> = packages
        .iter()
        .filter(|package| identifier(&package.name).is_some_and(|found| removed.contains(&found)))
        .map(|package| package.name.clone())
        .collect();
    names.extend(
        unused(packages, &images_after(packages, &removed))
            .into_iter()
            .filter(|package| !before.contains(package))
            .map(|package| package.name.clone()),
    );
    Ok(names)
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
            package("linux-image-generic", 10),
            package("linux-headers-generic", 10),
        ]
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
    fn kernels_are_listed_newest_first_with_their_size() {
        let found = kernels(&system(), &["linux-image-6.14.0-36-generic".into()], "6.14.0-35-generic");
        let names: Vec<&str> = found.iter().map(|kernel| kernel.identifier.as_str()).collect();
        assert_eq!(
            names,
            ["6.14.0-36-generic", "6.14.0-35-generic", "6.14.0-33-generic", "6.8.0-87-generic", "6.8.0-85-generic"]
        );
        assert!(!found[0].installed);
        assert!(found[1].running);
        assert_eq!(found[1].size, 15_000 + 500_000 + 30_000 + 80_000);
        assert_eq!(found[3].size, 14_000 + 400_000 + 150_000);
        assert_eq!(found[1].series(), "6.14");
        assert_eq!(fallback(&found).map(|kernel| kernel.identifier.as_str()), Some("6.14.0-33-generic"));
        let old: Vec<&str> = removable(&found).iter().map(|kernel| kernel.identifier.as_str()).collect();
        assert_eq!(old, ["6.8.0-87-generic", "6.8.0-85-generic"]);
    }

    #[test]
    fn leftovers_have_no_kernel() {
        let packages = system();
        let names: Vec<&str> = leftovers(&packages).iter().map(|package| package.name.as_str()).collect();
        assert_eq!(names, ["linux-headers-6.8.0-79", "linux-headers-6.8.0-79-generic"]);
    }

    #[test]
    fn removal_takes_the_modules_along_and_spares_the_running_kernel() {
        let packages = system();
        let running = "6.14.0-35-generic";
        assert!(removal(&packages, running, &[running.into()]).is_err());
        assert!(removal(&packages, running, &["6.1.0-1-generic".into()]).is_err());
        let names = removal(&packages, running, &["6.8.0-87-generic".into()]).unwrap_or_default();
        assert_eq!(
            names,
            ["linux-image-6.8.0-87-generic", "linux-modules-6.8.0-87-generic", "linux-modules-extra-6.8.0-87-generic"]
        );
        assert_eq!(size_of(&packages, &names), (14_000 + 400_000 + 150_000) * 1024);
    }

    #[test]
    fn only_newer_kernels_of_the_same_flavour_are_offered() {
        let names = [
            "linux-image-6.14.0-36-generic",
            "linux-image-6.14.0-37-generic",
            "linux-image-6.14.0-37-lowlatency",
            "linux-image-unsigned-6.14.0-37-generic",
            "linux-image-6.8.0-149-generic",
            "linux-image-6.17.0-5-generic",
        ];
        let offers = newer_offers(names.into_iter(), &system(), "6.14.0-35-generic");
        assert_eq!(offers, ["linux-image-6.17.0-5-generic", "linux-image-6.14.0-37-generic"]);
    }
}
