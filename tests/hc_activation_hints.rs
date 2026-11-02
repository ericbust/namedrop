#[path = "support/environment.rs"]
mod environment;
#[path = "support/hc_socket.rs"]
mod hc_socket;

use environment::CommandHome;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture {
    root: PathBuf,
    socket: Option<hc_socket::HcSocket>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("hints-{}", std::process::id()));
        fs::create_dir_all(root.join("home")).unwrap();
        let socket = Some(
            hc_socket::HcSocket::rpc(&root.join("hc.sock"), "628bcf8129aa037b1fa5aad3504926a6")
                .unwrap(),
        );
        fs::write(
            root.join("hc"),
            format!(
                r#"#!/usr/bin/python3
import sys
if sys.argv[1:] == ['server', 'status', '--json']:
    print({status:?})
elif sys.argv[1:] == ['ls', '--json']:
    print('{{"terminals":[]}}')
else:
    raise SystemExit('unexpected hc command '+str(sys.argv))
"#,
                status = include_str!("fixtures/hc/server-status.stdout")
            ),
        )
        .unwrap();
        fs::set_permissions(root.join("hc"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, socket }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        namedrop::host::hc::events::stop(
            &self.root.join("state"),
            "hc:628bcf8129aa037b1fa5aad3504926a6",
        )
        .unwrap();
        self.socket.take();
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn hc_activate_prints_app_next_steps_without_tui_commands() {
    let fixture = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_env(fixture.root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("HC_SOCKET", fixture.root.join("hc.sock"))
        .env("HC_BIN_PATH", fixture.root.join("hc"))
        .env("CODEX_HOME", fixture.root.join("codex"))
        .env("CLAUDE_CONFIG_DIR", fixture.root.join("claude"))
        .args([
            "--host",
            "hc",
            "--hc-space",
            "fixture-space",
            "--plugin-state",
        ])
        .arg(fixture.root.join("state"))
        .args([
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
            "--human",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(rendered.contains("Alt+N"), "{rendered}");
    assert!(
        rendered.contains("namedrop --host hc name @alias --type agent|terminal"),
        "{rendered}"
    );
    assert!(rendered.contains("namedrop --host hc doctor"), "{rendered}");
    assert!(
        rendered.contains("README: Install in Herdr app"),
        "{rendered}"
    );
    assert!(!rendered.contains("herdr integration"), "{rendered}");
}
