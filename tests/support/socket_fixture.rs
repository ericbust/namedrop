use std::{
    fs,
    io::ErrorKind,
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const PORTABLE_PATH_LIMIT: usize = 103;
const LONGEST_REGULAR_SUFFIX: &str = "/.runtime/herdr-core/default.sock";
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn socket_root(base: &Path, pid: u32, sequence: u64) -> PathBuf {
    let name = format!("n-{pid:x}-{sequence:x}");
    let candidate = base.join(&name);
    let root = if candidate.as_os_str().as_bytes().len() + LONGEST_REGULAR_SUFFIX.len()
        <= PORTABLE_PATH_LIMIT
    {
        candidate
    } else {
        Path::new("/tmp").join(name)
    };
    assert!(
        root.as_os_str().as_bytes().len() + LONGEST_REGULAR_SUFFIX.len() <= PORTABLE_PATH_LIMIT
    );
    root
}

pub fn short_socket_root() -> PathBuf {
    loop {
        let root = socket_root(
            &std::env::temp_dir(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create socket fixture {}: {error}", root.display()),
        }
    }
}

pub fn socket_path_fits(path: &Path, limit: usize) -> bool {
    path.as_os_str().as_bytes().len() <= limit
}

#[allow(dead_code)]
pub fn assert_socket_path(path: &Path) {
    let limit = if cfg!(target_os = "linux") { 107 } else { 103 };
    assert!(
        socket_path_fits(path, limit),
        "socket fixture exceeds {limit} bytes: {}",
        path.display()
    );
}
