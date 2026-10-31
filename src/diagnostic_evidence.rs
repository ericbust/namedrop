use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const DIRECTORY: &str = ".namedrop-diagnostic-evidence";
const MAX_EVENT: u64 = 64 * 1024;
static NEXT_EVENT: AtomicU64 = AtomicU64::new(0);

fn checked(file: File, directory: bool) -> io::Result<File> {
    let meta = file.metadata()?;
    if meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || if directory {
            !meta.is_dir() || meta.mode() & 0o777 != 0o700
        } else {
            !meta.is_file() || meta.nlink() != 1
        }
    {
        return Err(io::Error::other("unsafe diagnostic evidence file"));
    }
    Ok(file)
}

fn directory(root: &Path) -> io::Result<File> {
    checked(
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.join(DIRECTORY))?,
        true,
    )
}

fn open_at(directory: &File, name: &CString, flags: i32) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    checked(unsafe { File::from_raw_fd(fd) }, false)
}

/// Opt in one temporary fixture root, including child processes using its state.
pub fn enable(root: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join(DIRECTORY))
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    directory(root).map(|_| ())
}

pub(crate) fn record(state: &Path, event: Value) {
    for root in state.ancestors() {
        // Do not canonicalize: the supplied state may itself be the faulty path.
        if fs::symlink_metadata(root.join(DIRECTORY)).is_err() {
            continue;
        }
        let result = (|| -> io::Result<()> {
            let directory = directory(root)?;
            let sequence = NEXT_EVENT.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let pid = std::process::id();
            let bytes = serde_json::to_vec(&json!({
                "pid": pid, "sequence": sequence, "timestamp_ns": timestamp.to_string(),
                "state": state.to_string_lossy(), "event": event,
            }))?;
            let name = CString::new(format!("{timestamp}-{pid}-{sequence}.json"))?;
            open_at(
                &directory,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?
            .write_all(&bytes)
        })();
        if let Err(error) = result {
            let _ = writeln!(
                io::stdout().lock(),
                "diagnostic-write-failed root={} error={error:?}",
                root.display()
            );
        }
        return;
    }
}

/// Print on every fixture drop: libtest retains this even if a later assertion fails.
pub fn dump(root: &Path) {
    let dump_started_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let result = (|| -> io::Result<()> {
        let directory = directory(root)?;
        let mut entries = fs::read_dir(root.join(DIRECTORY))?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        println!(
            "diagnostic-evidence root={} records={} dump_pid={} dump_started_ns={}",
            root.display(),
            entries.len(),
            std::process::id(),
            dump_started_ns
        );
        for entry in entries {
            let result = (|| -> io::Result<Value> {
                let name = CString::new(entry.file_name().as_encoded_bytes())?;
                let file = open_at(&directory, &name, libc::O_RDONLY)?;
                let mut bytes = Vec::new();
                file.take(MAX_EVENT + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_EVENT {
                    return Err(io::Error::other("oversized evidence"));
                }
                Ok(serde_json::from_slice(&bytes)?)
            })();
            match result {
                Ok(event) => println!("diagnostic-event {event}"),
                Err(error) => println!(
                    "diagnostic-incomplete file={:?} error={error:?}",
                    entry.file_name()
                ),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        println!(
            "diagnostic-dump-failed root={} error={error:?}",
            root.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn independent_files_preserve_concurrent_events_and_refuse_symlinks() {
        let root = std::env::temp_dir().join(format!("namedrop-evidence-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        record(&root, json!({"not_opted_in": true}));
        assert!(!root.join(DIRECTORY).exists());
        enable(&root).unwrap();
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let root = &root;
                scope.spawn(move || {
                    for sequence in 0..16 {
                        record(
                            &root.join("missing/state"),
                            json!({"writer": writer, "sequence": sequence}),
                        );
                    }
                });
            }
        });
        let entries: Vec<_> = fs::read_dir(root.join(DIRECTORY))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(entries.len(), 128);
        for entry in &entries {
            assert_eq!(
                entry.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
            let value: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
            assert!(value["event"]["writer"].as_u64().unwrap() < 8);
        }
        fs::write(root.join(DIRECTORY).join("partial.json"), b"{").unwrap();
        fs::set_permissions(
            root.join(DIRECTORY).join("partial.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        symlink("/etc/passwd", root.join(DIRECTORY).join("symlink.json")).unwrap();
        dump(&root);
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        symlink(root.join(DIRECTORY), other.join(DIRECTORY)).unwrap();
        assert!(enable(&other).is_err());
        record(&other, json!({"must_not_be_written": true}));
        assert_eq!(fs::read_dir(root.join(DIRECTORY)).unwrap().count(), 130);
        fs::remove_file(other.join(DIRECTORY)).unwrap();
        enable(&other).unwrap();
        record(&other, json!({"isolated": true}));
        assert_eq!(fs::read_dir(other.join(DIRECTORY)).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join(DIRECTORY)).unwrap().count(), 130);
        fs::remove_dir_all(root).unwrap();
    }
}
