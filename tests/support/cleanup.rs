use std::{
    fs,
    io::{self, Write},
    path::Path,
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
