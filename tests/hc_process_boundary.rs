use namedrop::{HerdrCommandExecutor, ProcessHerdr, runtime::HostKind};
use std::{
    fs,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    process::Command,
};

#[test]
fn hc_client_and_daemon_child_drop_tui_environment_and_preserve_arguments() {
    let root = std::env::temp_dir().join(format!("namedrop-hc-boundary-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let fake = root.join("hc");
    fs::write(
        &fake,
        r#"#!/usr/bin/python3
import json, os, subprocess, sys
with open(os.environ['CLIENT_LOG'], 'w') as log:
    json.dump({'argv': sys.argv[1:], 'env': dict(os.environ)}, log)
args = sys.argv[1:]
os.environ.update(HERDR_SOCKET_PATH='daemon-socket', HERDR_PANE_ID='daemon-pane',
                  HERDR_UNKNOWN_DAEMON='daemon\nvalue $(touch never) *')
os.environb.update({b'HERDR_bad-name': b'bad', b'HERDR_nonutf8_\xff': b'bytes', b'HERDR_NEWLINE\n': b'newline'})
result = subprocess.run(args[args.index('--') + 1:], capture_output=True)
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "hc_process_boundary_child",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HC_BIN_PATH", &fake)
        .env("CLIENT_LOG", root.join("client.json"))
        .env("HERDR_SOCKET_PATH", "client-socket")
        .env("HERDR_PANE_ID", "client-pane")
        .env("HERDR_ARBITRARY_NEW_KEY", "client\nvalue")
        .env(
            std::ffi::OsString::from_vec(b"HERDR_client_nonutf8_\xff".to_vec()),
            "bytes",
        )
        .env("HC_TERMINAL_ID", "inherited-terminal")
        .env("HC_TERMINAL_SOCKET", "inherited-terminal-socket")
        .env("HC_SESSION", "kept-session")
        .env("HC_SOCKET", "kept-socket")
        .env("UNRELATED", "kept-value")
        .output()
        .unwrap();
    let observed: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("client.json")).unwrap()).unwrap();
    let client = observed["env"].as_object().unwrap();
    let leaked: Vec<_> = client
        .keys()
        .filter(|key| key.starts_with("HERDR_"))
        .collect();
    let child_output = String::from_utf8_lossy(&result.stdout);
    assert!(
        leaked.is_empty() && result.status.success(),
        "HC client retained HERDR keys {leaked:?}; target result {child_output}; stderr {}",
        String::from_utf8_lossy(&result.stderr)
    );
    for (key, expected) in [
        ("HC_SESSION", "kept-session"),
        ("HC_SOCKET", "kept-socket"),
        ("UNRELATED", "kept-value"),
    ] {
        assert_eq!(client[key], expected);
    }
    assert!(!client.contains_key("HC_TERMINAL_ID"));
    assert!(!client.contains_key("HC_TERMINAL_SOCKET"));
    assert_eq!(
        observed["argv"].as_array().unwrap()[..4],
        ["run", "--bg", "--no-parent", "--"].map(serde_json::Value::from)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore]
fn hc_process_boundary_child() {
    let arguments = [
        "",
        "spaces and 'quotes'",
        "$(touch never); *",
        "line one\nline two",
        "--flag=value",
    ];
    let mut argv = vec![
        "run",
        "--bg",
        "--no-parent",
        "--",
        "/usr/bin/python3",
        "-c",
        "import json,os,sys; print(json.dumps({'env':dict(os.environ),'argv':sys.argv[1:]}))",
    ];
    argv.extend(arguments);
    for invalid in [vec!["run", "--bg"], vec!["run", "--"]] {
        assert!(
            ProcessHerdr::for_host(HostKind::Hc)
                .execute(&invalid)
                .unwrap_err()
                .contains("requires an executable after --")
        );
    }
    let output = ProcessHerdr::for_host(HostKind::Hc)
        .with_socket(Some("must-not-be-injected".into()))
        .with_child_executable(env!("CARGO_BIN_EXE_namedrop").into())
        .execute(&argv)
        .unwrap();
    let target: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    let environment = target["env"].as_object().unwrap();
    let leaked: Vec<_> = environment
        .keys()
        .filter(|key| key.starts_with("HERDR_"))
        .collect();
    assert!(
        leaked.is_empty(),
        "HC daemon target retained HERDR keys {leaked:?}"
    );
    assert_eq!(target["argv"], serde_json::json!(arguments));
    assert_eq!(environment["HC_SESSION"], "kept-session");
    assert_eq!(environment["HC_SOCKET"], "kept-socket");
}

#[test]
fn hc_child_exec_preserves_non_utf8_arguments_and_requires_separator() {
    for args in [
        vec!["--hc-exec-child"],
        vec!["--hc-exec-child", "/bin/true"],
        vec!["--hc-exec-child", "--"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(64));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("requires -- followed by an executable")
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .args([
            "--hc-exec-child",
            "--",
            "/usr/bin/python3",
            "-c",
            "import os,sys; print(os.fsencode(sys.argv[1]).hex())",
        ])
        .arg(std::ffi::OsString::from_vec(
            b"original-argument-\xff".to_vec(),
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "6f726967696e616c2d617267756d656e742dff\n"
    );
}
