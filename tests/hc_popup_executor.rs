use namedrop::{
    HerdrCommandExecutor, ProcessHerdr, environment::EnvironmentSnapshot, runtime::HostKind,
};
use std::{
    fs,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt, net::UnixListener},
    time::{Duration, Instant},
};

#[test]
fn popup_executor_uses_captured_program_scrubbed_environment_and_bounded_children() {
    let root = std::env::temp_dir().join(format!("namedrop-popup-executor-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let socket = root.join("core.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let program = root.join(std::ffi::OsString::from_vec(b"app with spaces-\xff".to_vec()));
    let fallback = root.join("herdr-app");
    fs::write(&fallback, "#!/bin/sh\necho unexpected-PATH-fallback >&2\nexit 91\n").unwrap();
    fs::set_permissions(&fallback, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&program, "#!/usr/bin/python3\nimport json,os,sys,time\nif sys.argv[1] == 'slow': time.sleep(10)\nelif sys.argv[1] == 'fail': print('app refused',file=sys.stderr);sys.exit(7)\nelse: print(json.dumps({'argv':sys.argv[1:],'env':dict(os.environ)}))\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let snapshot = EnvironmentSnapshot::from_variables([
        ("PATH".into(), root.clone().into_os_string()),
        ("HERDR_APP_BIN_PATH".into(), program.into_os_string()),
        ("HC_SOCKET".into(), socket.clone().into_os_string()),
        ("HC_SESSION".into(), "popup-session".into()),
        ("HC_SCOPE".into(), "scope".into()),
        ("HC_RUNTIME".into(), "runtime".into()),
        ("HERDR_CUSTOM".into(), "tui".into()),
        ("NAMEDROP_HERDR_TIMEOUT_MS".into(), "80".into()),
    ]);
    let executor = ProcessHerdr::from_snapshot(HostKind::Hc, &snapshot);
    let output = executor
        .open_hc_popup(&["do", "mod.open", "space with 'quotes'"])
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(
        body["argv"],
        serde_json::json!(["do", "mod.open", "space with 'quotes'"])
    );
    assert_eq!(body["env"]["HC_SESSION"], "popup-session");
    assert_eq!(body["env"]["HC_SOCKET"], socket.to_str().unwrap());
    assert!(
        body["env"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.starts_with("HERDR_") && key != "HC_SCOPE" && key != "HC_RUNTIME")
    );
    let start = Instant::now();
    let error = executor.open_hc_popup(&["slow"]).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        executor.open_hc_popup(&["fail"]).unwrap_err(),
        "app refused"
    );
    drop(listener);
    fs::remove_file(socket).unwrap();
    assert!(
        executor
            .open_hc_popup(&["do"])
            .unwrap_err()
            .contains("core is unavailable")
    );
    fs::remove_dir_all(root).unwrap();
}
