#[path = "support/binary.rs"]
mod binary;

#[test]
fn standalone_publication_contracts() {
    let mut command = binary::command("python3");
    command
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/standalone_runtime.py"),
        ])
        .arg(binary::namedrop_path());
    if let Ok(case) = std::env::var("NAMEDROP_U8_TEST") {
        command.arg(case);
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "set NAMEDROP_TEST_STANDALONE_BASELINE and NAMEDROP_TEST_ARCHIVE to packaged fixture archives"]
fn standalone_archive_upgrade_lane() {
    let baseline = std::env::var("NAMEDROP_TEST_STANDALONE_BASELINE").unwrap();
    let out = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/update_lifecycle.py"),
            "--upgrade-from",
            &baseline,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!("{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
#[ignore = "set NAMEDROP_TEST_FIXTURE_OUTPUT to a fresh scratch directory"]
fn build_standalone_archive_fixtures() {
    let output = std::env::var("NAMEDROP_TEST_FIXTURE_OUTPUT").unwrap();
    let status = binary::command("python3")
        .args([
            "-I",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/build_standalone_upgrade.py"
            ),
            &output,
        ])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "requires an unrestricted integrator and disposable Rex server; set NAMEDROP_U8_REX_BINARY and NAMEDROP_U8_LIVE_OUTPUT"]
fn standalone_real_rex_runtime_lane() {
    let rex = std::env::var("NAMEDROP_U8_REX_BINARY").unwrap();
    let output = std::env::var("NAMEDROP_U8_LIVE_OUTPUT").unwrap();
    let status = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_install_live.py"),
        ])
        .args([binary::namedrop_path(), &rex, &output, "runtime"])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "requires unrestricted scratch Rex; set NAMEDROP_U8_REX_BINARY, NAMEDROP_U8_LIVE_OUTPUT and both fixture archives"]
fn standalone_real_rex_updates_lane() {
    let rex = std::env::var("NAMEDROP_U8_REX_BINARY").unwrap();
    let output = std::env::var("NAMEDROP_U8_LIVE_OUTPUT").unwrap();
    let status = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_install_live.py"),
        ])
        .args([binary::namedrop_path(), &rex, &output, "updates"])
        .status()
        .unwrap();
    assert!(status.success());
}
