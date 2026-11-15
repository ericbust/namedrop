#[path = "support/binary.rs"]
mod binary;

#[test]
fn standalone_installer_and_archive_contract_with_stub_hosts() {
    let output = binary::command("python3")
        .args(["-B", "-I"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/install_script.py"
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
