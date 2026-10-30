use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Command,
};

const EXECUTE: u64 = 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const READ: u64 = READ_FILE | READ_DIR;
const RUNTIME_DIRS: [&str; 4] = ["sqlite", "thread-writer-locks", "cache", "log"];

#[repr(C, packed)]
struct PathRule {
    access: u64,
    parent_fd: i32,
}

/// Probe the actual syscall, rather than guessing from the kernel version or
/// requiring access to root-only securityfs. Does not restrict the caller.
pub fn check_support() -> Result<(), String> {
    // SAFETY: a null pointer and zero size are the documented ABI query.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            1u32,
        )
    };
    if abi < 1 {
        return Err(format!(
            "Codex helpers require enabled Linux Landlock (kernel 5.13+): {}. Enable Landlock in the kernel/LSM configuration; the helper was not started",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Execute only a static native binary. Allowing a dynamic loader would also
/// let it load /bin/sh directly, bypassing the executable allowlist. Official
/// Codex Linux npm/release packages include static musl binaries.
fn static_elf(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 64];
    if file.read_exact(&mut header).is_err()
        || &header[..6] != b"\x7fELF\x02\x01"
        || u16::from_le_bytes([header[18], header[19]])
            != if cfg!(target_arch = "x86_64") {
                62
            } else {
                183
            }
        || !matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
    {
        return Ok(false);
    }
    let offset = u64::from_le_bytes(header[32..40].try_into().unwrap());
    let size = u16::from_le_bytes([header[54], header[55]]) as u64;
    let count = u16::from_le_bytes([header[56], header[57]]) as u64;
    if size != 56
        || count == 0
        || count > 1024
        || offset
            .checked_add(size * count)
            .is_none_or(|end| end > file.metadata().map(|m| m.len()).unwrap_or(0))
    {
        return Ok(false);
    }
    for index in 0..count {
        file.seek(SeekFrom::Start(offset + index * size))?;
        let mut kind = [0; 4];
        file.read_exact(&mut kind)?;
        if u32::from_le_bytes(kind) == 3 {
            // PT_INTERP
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn native_codex(entrypoint: &Path) -> Result<PathBuf, String> {
    if static_elf(entrypoint).unwrap_or(false) {
        return Ok(entrypoint.to_owned());
    }
    // Resolve the official npm layout without executing its JavaScript wrapper.
    // Both historical vendor/<triple>/codex and current vendor/<triple>/bin
    // packages are supported, including optional dependencies in npm/pnpm.
    let root = entrypoint.parent().and_then(Path::parent);
    if let Some(root) = root {
        let package = fs::read(root.join("package.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
        if package.as_ref().and_then(|p| p["name"].as_str()) == Some("@openai/codex") {
            let (triple, package) = if cfg!(target_arch = "x86_64") {
                ("x86_64-unknown-linux-musl", "codex-linux-x64")
            } else {
                ("aarch64-unknown-linux-musl", "codex-linux-arm64")
            };
            let mut roots = vec![root.to_owned()];
            if let Some(parent) = root.parent() {
                roots.push(parent.join(package));
            }
            for ancestor in root.ancestors() {
                roots.push(ancestor.join("node_modules/@openai").join(package));
            }
            for root in roots {
                for directory in ["bin", "codex"] {
                    let candidate = root
                        .join("vendor")
                        .join(triple)
                        .join(directory)
                        .join("codex");
                    if let Ok(candidate) = candidate.canonicalize()
                        && static_elf(&candidate).unwrap_or(false)
                    {
                        return Ok(candidate);
                    }
                }
            }
        }
    }
    Err("Codex helpers require the native static Linux Codex binary. Install the official @openai/codex npm package or musl release; shell wrappers and dynamically linked builds cannot preserve the execution sandbox".to_owned())
}

fn allow(ruleset: &File, path: &Path, access: u64) -> io::Result<()> {
    let path = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let access = if path.metadata()?.is_dir() {
        access
    } else {
        access & !READ_DIR
    };
    let rule = PathRule {
        access,
        parent_fd: path.as_raw_fd(),
    };
    // SAFETY: rule has the packed UAPI layout and both FDs remain open.
    if unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            1u32,
            &rule,
            0u32,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn prepare_home(home: &Path) -> io::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)?;
    let home = home.canonicalize()?;
    for name in RUNTIME_DIRS {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(home.join(name))?;
    }
    // Landlock grants existing inodes, not future paths. Reserve these before
    // confinement so Codex can open them read/write on the first helper call.
    // Codex fills an empty installation_id under its own file lock, in place.
    // Neither existing files nor auth.json contents are ever changed here.
    for name in ["auth.lock", "installation_id"] {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(home.join(name))
        {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !home.join(name).is_file() {
                    return Err(io::Error::other(format!(
                        "Codex {name} is not a regular file"
                    )));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(home)
}

fn allow_installation(ruleset: &File, binary: &Path) -> io::Result<()> {
    // Current standalone and npm bundles declare their data in this manifest.
    // Never grant the whole binary parent: a directly installed ~/codex would
    // otherwise expose HOME. Only known bundle data belonging to this binary
    // is readable; companion executables still cannot execute.
    for root in binary
        .parent()
        .into_iter()
        .chain(binary.parent().and_then(Path::parent))
    {
        let manifest = root.join("codex-package.json");
        let package = fs::read(&manifest)
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
        let Some(package) = package else { continue };
        if package["layoutVersion"] != 1 {
            continue;
        }
        let Some(entrypoint) = package["entrypoint"].as_str() else {
            continue;
        };
        if !["bin/codex", "codex/codex", "codex"].contains(&entrypoint)
            || root.join(entrypoint).canonicalize().ok().as_deref() != Some(binary)
        {
            continue;
        }
        allow(ruleset, &manifest, READ_FILE)?;
        for entry in ["codex-resources", "codex-path"] {
            allow(ruleset, &root.join(entry), READ)?;
        }
    }
    Ok(())
}

pub fn command(entrypoint: &Path, directory: &Path, codex_home: &Path) -> Result<Command, String> {
    check_support()?;
    let binary = native_codex(entrypoint)?;
    let build = || -> io::Result<Command> {
        let access = EXECUTE | READ;
        // SAFETY: ABI 1 ruleset consists of exactly this u64 field.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &access,
                size_of::<u64>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful create returns a new owned close-on-exec FD.
        let ruleset = unsafe { File::from_raw_fd(fd as i32) };
        for path in [
            "/usr",
            "/lib",
            "/lib64",
            "/etc",
            // Resolvers commonly symlink these into /run. Grant the resolved
            // file inode explicitly, without exposing the rest of /run.
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/dev/null",
            "/dev/random",
            "/dev/urandom",
            "/proc/meminfo",
            "/sys/devices/system/cpu",
        ] {
            allow(&ruleset, Path::new(path), READ)?;
        }
        allow(&ruleset, &binary, EXECUTE | READ_FILE)?;
        allow_installation(&ruleset, &binary)?;
        allow(&ruleset, directory, READ)?;
        let codex_home = prepare_home(codex_home)?;
        for entry in RUNTIME_DIRS
            .into_iter()
            .chain(["auth.json", "auth.lock", "installation_id"])
        {
            allow(&ruleset, &codex_home.join(entry), READ)?;
        }
        let mut command = Command::new(&binary);
        command.env("CODEX_HOME", &codex_home);
        for key in [
            "CODEX_MANAGED_PACKAGE_ROOT",
            "CODEX_MANAGED_BY_NPM",
            "CODEX_MANAGED_BY_PNPM",
            "CODEX_MANAGED_BY_BUN",
        ] {
            command.env_remove(key);
        }
        if binary != entrypoint {
            // native_codex validated this wrapper's @openai/codex package.
            let root = entrypoint.parent().and_then(Path::parent).unwrap();
            allow(&ruleset, &root.join("package.json"), READ_FILE)?;
            let manager = if root.components().any(|part| part.as_os_str() == ".pnpm") {
                "CODEX_MANAGED_BY_PNPM"
            } else if root.components().any(|part| part.as_os_str() == ".bun") {
                "CODEX_MANAGED_BY_BUN"
            } else {
                "CODEX_MANAGED_BY_NPM"
            };
            command
                .env("CODEX_MANAGED_PACKAGE_ROOT", root)
                .env(manager, "1");
        }
        // Dynamic-linker environment injection is irrelevant to static Codex,
        // but remove it explicitly so it cannot affect future launcher changes.
        command
            .env_remove("LD_PRELOAD")
            .env_remove("LD_LIBRARY_PATH");
        // SAFETY: this closure performs only prctl/syscall and errno capture,
        // with no allocation, locks, formatting, or policy construction after fork.
        // Failure is returned through Command::spawn; exec never runs uncontained.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0
                    || libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32)
                        < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(command)
    };
    build().map_err(|error| {
        format!(
            "Codex Linux containment could not be installed: {error}; the helper was not started"
        )
    })
}
