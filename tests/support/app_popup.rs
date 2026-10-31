use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub fn fake(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join("app-popup");
    fs::write(
        &path,
        r#"#!/usr/bin/python3
import json, os, sys
with open(os.path.join(os.path.dirname(os.fsencode(__file__)), b'popup.jsonl'), 'a') as log:
    json.dump({'argv': sys.argv[1:], 'env': {k.decode(errors='replace'): v.decode(errors='replace') if k in (b'HC_SESSION', b'HC_SOCKET', b'UNRELATED') else None for k, v in os.environb.items() if k.startswith(b'HERDR_') or k in (b'HC_TERMINAL_ID', b'HC_TERMINAL_SOCKET', b'HC_SESSION', b'HC_SOCKET', b'UNRELATED')}}, log)
    log.write('\n')
print('{"opened":true}')
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

pub fn calls(root: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(root.join("popup.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub fn assert_tour(root: &Path, count: usize) {
    let calls = calls(root);
    assert_eq!(calls.len(), count, "app popup calls: {calls:?}");
    for call in calls {
        assert_eq!(
            call["argv"],
            serde_json::json!(["do", "mod.open", "--name", "namedrop-tour"])
        );
        let env = call["env"].as_object().unwrap();
        assert!(env.keys().all(|key| !key.starts_with("HERDR_")), "{env:?}");
        assert!(!env.contains_key("HC_TERMINAL_ID"));
        assert!(!env.contains_key("HC_TERMINAL_SOCKET"));
    }
}

pub fn assert_one_test(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn tripwire(root: &Path) -> PathBuf {
    let path = root.join("path");
    fs::create_dir_all(&path).unwrap();
    let stub = path.join("herdr-app");
    fs::write(
        &stub,
        r#"#!/usr/bin/python3
import os, sys
with open(os.path.join(os.path.dirname(__file__), 'unexpected-app.log'), 'a') as log:
    log.write(repr(sys.argv[1:]) + '\n')
sys.exit(97)
"#,
    )
    .unwrap();
    fs::set_permissions(stub, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

pub fn guard(cases: &[&str], label: &str) {
    let root = std::env::temp_dir().join(format!(
        "namedrop-popup-guard-{label}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = tripwire(&root);
    let mut failures = Vec::new();
    for case in cases {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", case, "--nocapture"])
            .env_remove("HERDR_APP_BIN_PATH")
            .env(
                "PATH",
                std::env::join_paths([path.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                    .unwrap(),
            )
            .output()
            .unwrap();
        let unexpected = fs::read_to_string(path.join("unexpected-app.log")).unwrap_or_default();
        if !unexpected.is_empty() {
            failures.push(format!("{case} invoked PATH herdr-app: {unexpected}"));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            failures.push(format!(
                "{case}: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        } else {
            assert_one_test(&output);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    fs::remove_dir_all(root).unwrap();
}
