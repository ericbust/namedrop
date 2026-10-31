mod support;

use std::process::{Command, Stdio};
use support::TestDirectory;

const RECORD: &str = "pane-effects: retrying directory open after ENOENT for pane source=runtime\n";

#[test]
#[ignore]
fn fixture_cleanup_child() {
    assert!(std::env::var_os("NAMEDROP_RETRY_DIAGNOSTIC_CHILD").is_some());
    let directory = TestDirectory::new();
    let path = directory.path().to_owned();
    std::fs::write(path.join("pane-effects-retries.log"), RECORD).unwrap();
    drop(directory);
    assert!(!path.exists());
}

#[test]
fn fixture_cleanup_preserves_retry_records_in_raw_stderr() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fixture_cleanup_child", "--ignored"])
        .env("NAMEDROP_RETRY_DIAGNOSTIC_CHILD", "1")
        .stdout(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), RECORD);
}
