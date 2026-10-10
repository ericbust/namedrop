#[test]
fn guided_update_fake_host_lifecycle() {
    let output = std::process::Command::new("python3")
        .env("NAMEDROP_TEST_BINARY", env!("CARGO_BIN_EXE_namedrop"))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/update_lifecycle.py"
        ))
        .output()
        .expect("Python 3 is required for the guided update lifecycle check");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
