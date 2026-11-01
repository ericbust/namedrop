use std::{
    ffi::{CStr, CString, OsString},
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

pub(super) struct Dir {
    file: File,
    pub path: PathBuf,
}

fn name(value: &str) -> io::Result<CString> {
    if value.is_empty() || value.contains('/') || value == "." || value == ".." {
        return Err(io::Error::other("invalid spool filename"));
    }
    CString::new(value).map_err(io::Error::other)
}

fn checked(result: libc::c_int) -> io::Result<libc::c_int> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

impl Dir {
    pub fn open(path: &Path) -> io::Result<Self> {
        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(io::Error::other(
                "spool directory cannot contain parent components",
            ));
        }
        let path: PathBuf = path.components().collect();
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        Ok(Self { file, path })
    }

    pub fn child(&self, leaf: &str) -> io::Result<Self> {
        let leaf_c = name(leaf)?;
        let made = unsafe { libc::mkdirat(self.file.as_raw_fd(), leaf_c.as_ptr(), 0o700) };
        if made == 0 {
            self.file.sync_all()?;
        } else if io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io::Error::last_os_error());
        }
        let fd = checked(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                leaf_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
            path: self.path.join(leaf),
        })
    }

    fn regular(&self, leaf: &str, flags: i32) -> io::Result<File> {
        let leaf = name(leaf)?;
        let fd = checked(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                leaf.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("deferred record is not a regular file"));
        }
        Ok(file)
    }

    pub fn lock(&self, leaf: &str) -> io::Result<File> {
        match self.regular(leaf, libc::O_RDWR | libc::O_CREAT) {
            Ok(file) => Ok(file),
            Err(error) => {
                self.remove(leaf)?;
                Err(error)
            }
        }
    }

    pub fn read(&self, leaf: &str) -> io::Result<Vec<u8>> {
        let mut file = self.regular(leaf, libc::O_RDONLY)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn check_regular(&self, leaf: &str) -> io::Result<()> {
        self.regular(leaf, libc::O_RDONLY).map(drop)
    }

    pub fn contains(&self, leaf: &str) -> io::Result<bool> {
        let leaf = name(leaf)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                leaf.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        match checked(result) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn remove(&self, leaf: &str) -> io::Result<()> {
        let leaf = name(leaf)?;
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), leaf.as_ptr(), 0) };
        match checked(result) {
            Ok(_) => self.file.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error)
                if error.raw_os_error() == Some(libc::EISDIR)
                    || error.raw_os_error() == Some(libc::EPERM) =>
            {
                checked(unsafe {
                    libc::unlinkat(self.file.as_raw_fd(), leaf.as_ptr(), libc::AT_REMOVEDIR)
                })?;
                self.file.sync_all()
            }
            Err(error) => Err(error),
        }
    }

    pub fn publish(&self, leaf: &str, bytes: &[u8], temporary: &str) -> io::Result<()> {
        let destination = name(leaf)?;
        let source = name(temporary)?;
        let mut file = self.regular(temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            let original = file.metadata()?;
            match checked(unsafe {
                libc::linkat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    self.file.as_raw_fd(),
                    destination.as_ptr(),
                    0,
                )
            }) {
                Ok(_) => {
                    self.check_publication(leaf, Some(&original))?;
                    self.file.sync_all()
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.check_publication(leaf, None)
                }
                Err(error) => Err(error),
            }
        })();
        self.remove(temporary)?;
        result
    }

    fn check_publication(
        &self,
        leaf: &str,
        original: Option<&std::fs::Metadata>,
    ) -> io::Result<()> {
        let result = self.regular(leaf, libc::O_RDONLY).and_then(|file| {
            let actual = file.metadata()?;
            if original.is_some_and(|original| {
                original.dev() != actual.dev() || original.ino() != actual.ino()
            }) {
                return Err(io::Error::other(
                    "temporary publication record was replaced",
                ));
            }
            Ok(())
        });
        if result.is_err() {
            self.remove(leaf)?;
        }
        result
    }

    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let from = name(from)?;
        let to = name(to)?;
        checked(unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                from.as_ptr(),
                self.file.as_raw_fd(),
                to.as_ptr(),
            )
        })?;
        self.file.sync_all()
    }

    pub fn entries(&self) -> io::Result<Vec<OsString>> {
        let dot = CString::new(".").unwrap();
        let fd = checked(unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        let file = unsafe { File::from_raw_fd(fd) };
        let stream = unsafe { libc::fdopendir(file.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _ = file.into_raw_fd();
        let mut entries = Vec::new();
        let read_result = loop {
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                break if error.raw_os_error() == Some(0) {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes != b"." && bytes != b".." {
                entries.push(OsString::from_vec(bytes.to_vec()));
            }
        };
        let close_result = checked(unsafe { libc::closedir(stream) });
        read_result?;
        close_result?;
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_does_not_unlink_existing_lock() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("namedrop-lock-permission-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("worker.lock");
        std::fs::write(&path, b"held lock").unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let error = Dir::open(&root).unwrap().lock("worker.lock").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(std::fs::read(&path).unwrap(), b"held lock");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_leaf_aliases_are_refused_but_parent_aliases_work() {
        let root = std::env::temp_dir().join(format!("namedrop-dir-leaf-{}", std::process::id()));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::os::unix::fs::symlink(root.join("state"), root.join("leaf")).unwrap();
        for suffix in ["", "/", "/."] {
            assert!(
                Dir::open(&PathBuf::from(format!(
                    "{}{suffix}",
                    root.join("leaf").display()
                )))
                .is_err()
            );
        }
        std::os::unix::fs::symlink(&root, root.join("parent")).unwrap();
        let dir = Dir::open(&root.join("parent/state")).unwrap();
        dir.publish("record", b"owned", ".temporary").unwrap();
        assert_eq!(std::fs::read(root.join("state/record")).unwrap(), b"owned");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refused_publication_targets_are_unlinked_without_touching_their_target() {
        let root =
            std::env::temp_dir().join(format!("namedrop-dir-publish-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("victim"), b"untouched").unwrap();
        let dir = Dir::open(&root).unwrap();
        for fifo in [false, true] {
            if fifo {
                assert_eq!(
                    unsafe {
                        libc::mkfifoat(
                            dir.file.as_raw_fd(),
                            name("record").unwrap().as_ptr(),
                            0o600,
                        )
                    },
                    0
                );
            } else {
                std::os::unix::fs::symlink(root.join("victim"), root.join("record")).unwrap();
            }
            assert!(dir.publish("record", b"owned", ".temporary").is_err());
            assert!(std::fs::symlink_metadata(root.join("record")).is_err());
            assert_eq!(std::fs::read(root.join("victim")).unwrap(), b"untouched");
            dir.publish("record", b"owned", ".temporary").unwrap();
            assert_eq!(dir.read("record").unwrap(), b"owned");
            dir.remove("record").unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn descriptor_stays_in_original_directory_after_name_is_replaced() {
        let root = std::env::temp_dir().join(format!("namedrop-dir-swap-{}", std::process::id()));
        std::fs::create_dir_all(root.join("queue")).unwrap();
        std::fs::create_dir_all(root.join("victim")).unwrap();
        let dir = Dir::open(&root.join("queue")).unwrap();
        std::fs::write(root.join("victim/record"), b"untouched").unwrap();
        std::fs::rename(root.join("queue"), root.join("original")).unwrap();
        std::os::unix::fs::symlink(root.join("victim"), root.join("queue")).unwrap();
        dir.publish("record", b"owned", ".temporary").unwrap();
        assert_eq!(dir.read("record").unwrap(), b"owned");
        assert!(dir.entries().unwrap().contains(&OsString::from("record")));
        dir.rename("record", "renamed").unwrap();
        dir.remove("renamed").unwrap();
        assert_eq!(
            std::fs::read(root.join("victim/record")).unwrap(),
            b"untouched"
        );
        assert!(
            std::fs::read_dir(root.join("original"))
                .unwrap()
                .next()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
