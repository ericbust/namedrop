#[path = "support/binary.rs"]
mod binary;
#[test]
fn herdr_runtime_activation_contracts() {
    let out = binary::command("python3")
        .arg("-I")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/runtime_activation.py"
        ))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
