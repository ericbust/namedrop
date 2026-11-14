#[path = "support/binary.rs"]
mod binary;
#[test]
fn every_shared_package_writer_is_covered_by_the_installed_runtime_matrix() {
    let out = binary::command("python3")
        .arg("-I")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/package_lease.py"
        ))
        .arg(binary::namedrop_path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
