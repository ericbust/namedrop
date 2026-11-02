use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn remove_fixture(root: &Path) -> io::Result<()> {
    copy_retry_records(root);
    fs::remove_dir_all(root)
}

fn copy_retry_records(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_retry_records(&entry.path());
        } else if kind.is_file()
            && entry.file_name() == "pane-effects-retries.log"
            && let Ok(record) = fs::read(entry.path())
        {
            let _ = io::stderr().lock().write_all(&record);
        }
    }
}

#[allow(dead_code)]
pub struct FixtureDirectory(pub PathBuf);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        if let Err(error) = remove_fixture(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("remove fixture {}: {error}", self.0.display());
        }
    }
}
