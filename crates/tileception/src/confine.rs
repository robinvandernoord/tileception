//! Confinement for a plugin's process, for manifests that ask for it with
//! `confined = true` in their `[dashboard]` block. The dashboard protocol
//! limits what a plugin can do to the desktop; this limits what its process
//! can do to the user's files and the network.
//!
//! Two kernel features do the work, both applied in the child between fork
//! and exec, without privileges and without a helper program:
//!
//! - Landlock leaves the process the files in its [`Allowance`]: system
//!   folders and the plugin's own to read and run, a private folder to write.
//! - A seccomp filter refuses new sockets, so nothing leaves over the
//!   network, D-Bus or another display. The plugin keeps what it was
//!   started with: its pipes to Tileception and its dashboard connection.
//!
//! A kernel without Landlock cannot confine, and a plugin that asked for
//! confinement is then not started at all.

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Command,
};

/// Landlock's file rights, as `linux/landlock.h` numbers them.
mod access {
    pub const EXECUTE: u64 = 1 << 0;
    pub const WRITE_FILE: u64 = 1 << 1;
    pub const READ_FILE: u64 = 1 << 2;
    pub const READ_DIR: u64 = 1 << 3;
    pub const REMOVE_DIR: u64 = 1 << 4;
    pub const REMOVE_FILE: u64 = 1 << 5;
    pub const MAKE_DIR: u64 = 1 << 7;
    pub const MAKE_REG: u64 = 1 << 8;
    pub const MAKE_FIFO: u64 = 1 << 10;
    pub const MAKE_SYM: u64 = 1 << 12;
    /// Everything the first Landlock handles, up to `MAKE_SYM`.
    pub const FIRST: u64 = (1 << 13) - 1;
    /// From the second Landlock on.
    pub const REFER: u64 = 1 << 13;
    /// From the third.
    pub const TRUNCATE: u64 = 1 << 14;
    /// From the fifth.
    pub const IOCTL_DEV: u64 = 1 << 15;

    /// The rights a rule on a file, not a folder, may name.
    pub const OF_FILES: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;
    pub const READ: u64 = EXECUTE | READ_FILE | READ_DIR;
    /// A folder of the plugin's own: everything but running what it wrote
    /// there and making device nodes or sockets.
    pub const WRITE: u64 = READ_FILE
        | READ_DIR
        | WRITE_FILE
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_DIR
        | MAKE_REG
        | MAKE_FIFO
        | MAKE_SYM
        | REFER
        | TRUNCATE;
    pub const DEVICE: u64 = READ_FILE | READ_DIR | WRITE_FILE | IOCTL_DEV;
}

/// TCP binds and connects, from the fourth Landlock on. The seccomp filter
/// already refuses the sockets; this holds for one handed over later too.
const NET_TCP: u64 = (1 << 0) | (1 << 1);
/// Abstract Unix sockets and signals outside the plugin's own processes,
/// from the sixth Landlock on.
const SCOPES: u64 = (1 << 0) | (1 << 1);

const CREATE_RULESET_VERSION: u32 = 1 << 0;
const RULE_PATH_BENEATH: libc::c_int = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// System folders every program reads to start: its interpreter and
/// libraries, fonts, certificates and settings. Read and run, never write.
const SYSTEM: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/opt",
    "/nix/store",
    "/var/cache/fontconfig",
    "/sys",
];

/// Devices any program may need, and the GPU for a plugin that draws with it.
const DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/full", "/dev/random", "/dev/urandom", "/dev/dri"];

/// What a confined plugin may touch besides the system folders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Allowance {
    /// Files and folders to read and run: where the plugin is installed.
    pub read: Vec<PathBuf>,
    /// The plugin's private folder, the only place it may write.
    pub private: PathBuf,
}

impl Allowance {
    /// The allowance of the plugin `plugin_id`, started as `command` with
    /// `args`. `path` is the `PATH` a bare command name is looked up in.
    pub fn for_plugin(plugin_id: &str, command: &str, args: &[String], path: Option<&OsStr>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut read = Vec::new();
        if let Some(program) = find_program(command, path) {
            read.extend(installed_at(&program, home.as_deref()));
        }
        // `python3 /path/to/plugin.py`: the script's folder as well.
        for arg in args {
            let file = Path::new(arg);
            if file.is_absolute() && file.is_file() {
                read.extend(installed_at(file, home.as_deref()));
            }
        }
        read.sort();
        read.dedup();
        Self { read, private: private_dir(plugin_id) }
    }

    /// Confines `process` when it starts: creates the private folder,
    /// points the plugin at it, and installs the Landlock rules and the
    /// seccomp filter. An error means the process must not be started.
    pub fn apply(&self, process: &mut Command) -> Result<(), String> {
        let filter = socket_filter()?;
        let private = &self.private;
        let cache = private.join("cache");
        let run = private.join("run");
        let tmp = private.join("tmp");
        for dir in [private, &cache, &run, &tmp] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|error| format!("{}: {error}", dir.display()))?;
        }
        let ruleset = self.ruleset()?;
        // Whatever a program writes without being told where lands in the
        // private folder: caches, temporary files, and the buffers Wayland
        // clients keep in the runtime folder. The session's own runtime
        // folder, with its sockets, is out of reach either way.
        process
            .env("TILECEPTION_PLUGIN_DATA", private)
            .env("XDG_CACHE_HOME", cache)
            .env("XDG_RUNTIME_DIR", run)
            .env("TMPDIR", tmp);
        #[allow(unsafe_code, reason = "the plugin confines itself between fork and exec")]
        // SAFETY: only prctl and two raw system calls run between fork and
        // exec, all async-signal-safe, on a descriptor and a filter this
        // closure owns. Nothing allocates.
        unsafe {
            process.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let program =
                    libc::sock_fprog { len: filter.len() as libc::c_ushort, filter: filter.as_ptr().cast_mut() };
                if libc::syscall(libc::SYS_seccomp, libc::SECCOMP_SET_MODE_FILTER, 0, &raw const program) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    /// A Landlock ruleset holding this allowance, for the child to enter.
    fn ruleset(&self) -> Result<OwnedFd, String> {
        let abi = landlock_abi();
        if abi < 1 {
            return Err("this kernel has no Landlock (Linux 5.13 or later, with landlock in its lsm= list), \
                        so the plugin cannot be confined and is not started; \
                        confined = false in its [dashboard] block runs it without"
                .into());
        }
        let mut handled = access::FIRST;
        for (since, right) in [(2, access::REFER), (3, access::TRUNCATE), (5, access::IOCTL_DEV)] {
            if abi >= since {
                handled |= right;
            }
        }
        let attr = RulesetAttr {
            handled_access_fs: handled,
            handled_access_net: if abi >= 4 { NET_TCP } else { 0 },
            scoped: if abi >= 6 { SCOPES } else { 0 },
        };
        #[allow(unsafe_code, reason = "Landlock has no wrapper in libc")]
        // SAFETY: the attribute struct outlives the call and its size is
        // passed with it; an older kernel accepts the zeroed newer fields.
        let created = unsafe {
            libc::syscall(libc::SYS_landlock_create_ruleset, &raw const attr, std::mem::size_of::<RulesetAttr>(), 0)
        };
        if created < 0 {
            return Err(format!("Landlock ruleset: {}", std::io::Error::last_os_error()));
        }
        #[allow(unsafe_code, reason = "takes ownership of the new ruleset descriptor")]
        // SAFETY: the kernel just returned this descriptor, close-on-exec,
        // and nothing else holds it.
        let ruleset = unsafe { OwnedFd::from_raw_fd(created as i32) };
        let system = SYSTEM.iter().map(|path| (Path::new(*path), access::READ));
        let own = self.read.iter().map(|path| (path.as_path(), access::READ));
        let devices = DEVICES.iter().map(|path| (Path::new(*path), access::DEVICE));
        for (path, access) in system.chain(own).chain(devices).chain([(self.private.as_path(), access::WRITE)]) {
            // What is not there needs no rule: /lib32, /nix, a machine without a GPU.
            let Ok(file) =
                std::fs::OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_CLOEXEC).open(path)
            else {
                continue;
            };
            let folder = file.metadata().is_ok_and(|metadata| metadata.is_dir());
            let access = access & handled & if folder { u64::MAX } else { access::OF_FILES };
            add_rule(&ruleset, &file, access)
                .map_err(|error| format!("Landlock rule for {}: {error}", path.display()))?;
        }
        Ok(ruleset)
    }
}

fn add_rule(ruleset: &OwnedFd, file: &File, access: u64) -> std::io::Result<()> {
    if access == 0 {
        return Ok(());
    }
    let attr = PathBeneathAttr { allowed_access: access, parent_fd: file.as_raw_fd() };
    #[allow(unsafe_code, reason = "Landlock has no wrapper in libc")]
    // SAFETY: both descriptors are open for the call and the attribute
    // struct outlives it.
    let added = unsafe {
        libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &raw const attr, 0)
    };
    if added == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// The kernel's Landlock version; 0 without Landlock.
pub fn landlock_abi() -> i64 {
    #[allow(unsafe_code, reason = "Landlock has no wrapper in libc")]
    // SAFETY: with the version flag the call takes no attribute and creates nothing.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    abi.max(0)
}

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: Option<u32> = Some(0xC000_003E);
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: Option<u32> = Some(0xC000_00B7);
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const AUDIT_ARCH: Option<u32> = None;

/// A seccomp filter that refuses what would open a way out: `socket` for
/// any family, `io_uring_setup` (which can make sockets too) and datagram
/// socket pairs (which can send to any Unix socket by its path). Stream
/// socket pairs, and sockets the plugin was started with, keep working.
fn socket_filter() -> Result<Vec<libc::sock_filter>, String> {
    // Classic BPF: load a word, jump if equal or greater, and, return.
    const LOAD: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const JUMP_AT_LEAST: u16 = 0x35;
    const AND: u16 = 0x54;
    const RETURN: u16 = 0x06;
    // Offsets in `struct seccomp_data`; an argument's low half comes first
    // on the little-endian machines this is built for.
    const NUMBER: u32 = 0;
    const ARCH: u32 = 4;
    const SECOND_ARGUMENT: u32 = 16 + 8;
    const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
    const REFUSE: u32 = libc::SECCOMP_RET_ERRNO | libc::EACCES as u32;
    const NO_SUCH_CALL: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;

    let arch =
        AUDIT_ARCH.ok_or("confining plugins is not supported on this architecture, so the plugin is not started")?;
    let step = |code, when_true, when_false, value| libc::sock_filter { code, jt: when_true, jf: when_false, k: value };
    Ok(vec![
        // Another instruction set (32-bit calls on x86-64) numbers its
        // calls differently and would slip past the checks below.
        step(LOAD, 0, 0, ARCH),
        step(JUMP_EQUAL, 1, 0, arch),
        step(RETURN, 0, 0, NO_SUCH_CALL),
        step(LOAD, 0, 0, NUMBER),
        // So would the x32 calls, numbered from this bit up.
        step(JUMP_AT_LEAST, 0, 1, 0x4000_0000),
        step(RETURN, 0, 0, NO_SUCH_CALL),
        step(JUMP_EQUAL, 6, 0, libc::SYS_socket as u32),
        step(JUMP_EQUAL, 5, 0, libc::SYS_io_uring_setup as u32),
        step(JUMP_EQUAL, 0, 3, libc::SYS_socketpair as u32),
        step(LOAD, 0, 0, SECOND_ARGUMENT),
        step(AND, 0, 0, 0xf),
        step(JUMP_EQUAL, 1, 0, libc::SOCK_DGRAM as u32),
        step(RETURN, 0, 0, ALLOW),
        step(RETURN, 0, 0, REFUSE),
    ])
}

/// `$XDG_STATE_HOME/tileception/plugins/<id>`: the one folder a confined
/// plugin may write, kept between runs.
pub fn private_dir(plugin_id: &str) -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/plugins").join(plugin_id)
}

/// The file `command` names: itself with a slash in it, else the first
/// executable of that name in `path`.
fn find_program(command: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    if command.contains('/') {
        return Some(PathBuf::from(command));
    }
    let path = path.map(OsString::from).or_else(|| std::env::var_os("PATH"))?;
    std::env::split_paths(&path).map(|dir| dir.join(command)).find(|file| file.is_file())
}

/// What a program at `file` needs to read to run, beyond the system
/// folders: the Python environment it is installed in (with the Python
/// that environment is made from), else the folder it lies in. A program
/// directly in the home folder or in a shared `bin` gets only itself, so
/// that being installed there does not open everything beside it.
fn installed_at(file: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    // What a link points at is what runs; following the link itself needs no rule.
    let Ok(real) = file.canonicalize() else { return Vec::new() };
    if SYSTEM.iter().any(|system| real.starts_with(system)) {
        return Vec::new();
    }
    let Some(folder) = real.parent() else { return vec![real] };
    if folder.file_name().is_some_and(|name| name == "bin")
        && let Some(prefix) = folder.parent()
        && prefix.join("pyvenv.cfg").is_file()
    {
        let mut read = vec![prefix.to_path_buf()];
        // The environment's `python` links to the Python it was made
        // from, which uv keeps outside the system folders.
        if let Ok(python) = folder.join("python").canonicalize()
            && !python.starts_with(prefix)
        {
            read.extend(installed_at(&python, home));
        }
        return read;
    }
    // A Python of uv's own: `<install>/bin/python3.x` with its library beside `bin`.
    if folder.file_name().is_some_and(|name| name == "bin")
        && let Some(prefix) = folder.parent()
        && prefix.join("lib").is_dir()
        && real.file_name().is_some_and(|name| name.to_string_lossy().starts_with("python"))
        && Some(prefix) != home
        && home.is_none_or(|home| prefix != home.join(".local"))
    {
        return vec![prefix.to_path_buf()];
    }
    let shared =
        folder.file_name().is_some_and(|name| name == "bin") || Some(folder) == home || folder.parent().is_none();
    if shared { vec![real] } else { vec![folder.to_path_buf()] }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use std::{net::TcpListener, process::Stdio};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-confine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_plugin_reads_where_it_is_installed_and_no_wider() {
        let dir = scratch("installed");
        let home = dir.join("home");
        let touch = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        };
        // A folder of its own.
        let own = home.join("plugins/lamp/lamp.py");
        touch(&own);
        assert_eq!(installed_at(&own, Some(&home)), vec![home.join("plugins/lamp")]);
        // Directly in the home folder, or in a shared bin: the file alone.
        let loose = home.join("plugin.py");
        touch(&loose);
        assert_eq!(installed_at(&loose, Some(&home)), vec![loose.clone()]);
        let shared = home.join(".local/bin/tool");
        touch(&shared);
        assert_eq!(installed_at(&shared, Some(&home)), vec![shared.clone()]);
        // A Python environment, reached through a link in the shared bin,
        // with the Python it was made from.
        let python = home.join(".local/share/uv/python/cpython-3.13/bin/python3.13");
        touch(&python);
        std::fs::create_dir_all(home.join(".local/share/uv/python/cpython-3.13/lib")).unwrap();
        let venv = home.join(".local/uvenv/venvs/weather");
        touch(&venv.join("pyvenv.cfg"));
        touch(&venv.join("bin/weather"));
        std::os::unix::fs::symlink(&python, venv.join("bin/python")).unwrap();
        let link = home.join(".local/bin/weather");
        std::os::unix::fs::symlink(venv.join("bin/weather"), &link).unwrap();
        assert_eq!(
            installed_at(&link, Some(&home)),
            vec![venv.clone(), home.join(".local/share/uv/python/cpython-3.13")]
        );
        // System programs need nothing more, and a missing one nothing at all.
        assert_eq!(installed_at(Path::new("/usr/bin/env"), Some(&home)), Vec::<PathBuf>::new());
        assert_eq!(installed_at(&home.join("gone"), Some(&home)), Vec::<PathBuf>::new());
        // A bare name is looked up in the given PATH; a script argument counts.
        let path = std::env::join_paths([home.join(".local/bin")]).unwrap();
        let allowance = Allowance::for_plugin("weather", "weather", &[own.to_string_lossy().into_owned()], Some(&path));
        assert!(allowance.read.contains(&venv) && allowance.read.contains(&home.join("plugins/lamp")));
        assert!(allowance.private.ends_with("tileception/plugins/weather"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_socket_filter_jumps_stay_inside_it() {
        let filter = socket_filter().unwrap();
        for (index, step) in filter.iter().enumerate() {
            if step.code == 0x15 || step.code == 0x35 {
                assert!(index + 1 + usize::from(step.jt.max(step.jf)) < filter.len());
            }
        }
        assert_eq!(filter.last().unwrap().code, 0x06);
    }

    /// Runs `script` in bash, confined to `allowance`.
    fn confined(allowance: &Allowance, script: &str) -> std::process::Output {
        let mut process = Command::new("/bin/bash");
        process.args(["-c", script]).stdin(Stdio::null());
        allowance.apply(&mut process).unwrap();
        process.output().unwrap()
    }

    #[test]
    fn a_confined_process_keeps_to_its_allowance() {
        if landlock_abi() < 1 || !Path::new("/bin/bash").exists() {
            eprintln!("skipped: this kernel has no Landlock, or there is no bash");
            return;
        }
        let dir = scratch("spawn");
        let own = dir.join("own");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(own.join("data"), "mine").unwrap();
        std::fs::write(outside.join("secret"), "theirs").unwrap();
        let allowance = Allowance { read: vec![own.clone()], private: dir.join("private") };
        let allowed = |script: &str| {
            let output = confined(&allowance, script);
            assert!(output.status.success(), "{script}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let refused = |script: &str| {
            let output = confined(&allowance, script);
            assert!(!output.status.success(), "{script} was not refused");
        };

        // Its own files and the system's read; its private folder writes.
        assert_eq!(allowed(&format!("cat {}", own.join("data").display())), "mine");
        allowed("cat /etc/passwd > /dev/null && ls /usr/bin > /dev/null");
        allowed(
            "echo kept > \"$TILECEPTION_PLUGIN_DATA/note\" && echo t > \"$TMPDIR/t\" && echo r > \"$XDG_RUNTIME_DIR/r\"",
        );
        assert_eq!(std::fs::read_to_string(dir.join("private/note")).unwrap(), "kept\n");

        // Nothing else reads, lists or writes: not beside it, not at home.
        refused(&format!("cat {}", outside.join("secret").display()));
        refused(&format!("ls {}", outside.display()));
        refused(&format!("echo x > {}", outside.join("new").display()));
        refused(&format!("echo x > {}", own.join("data").display()));
        refused(&format!("echo x >> {}", outside.join("secret").display()));
        refused("ls \"$HOME\"");
        refused("ls /tmp");
        assert!(!outside.join("new").exists());
        assert_eq!(std::fs::read_to_string(outside.join("secret")).unwrap(), "theirs");
        assert_eq!(std::fs::read_to_string(own.join("data")).unwrap(), "mine");

        // No network: a listener on this machine is never reached.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        refused(&format!("exec 3<>/dev/tcp/127.0.0.1/{port}"));
        refused(&format!("exec 3<>/dev/udp/127.0.0.1/{port}"));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        // The same connection succeeds unconfined, so the check means something.
        let free =
            Command::new("/bin/bash").args(["-c", &format!("exec 3<>/dev/tcp/127.0.0.1/{port}")]).status().unwrap();
        assert!(free.success());
        assert!(listener.accept().is_ok());

        // What it starts stays confined.
        refused(&format!("bash -c 'cat {}'", outside.join("secret").display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
