//! The tailnet link as a QR code, printed by `channels --tailscale --no-open`.
//! Tailscale and Herdr are fakes throughout.
#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/executable.rs"]
mod executable;
#[path = "support/scan.rs"]
mod scan;

use scan::{Theme, scan};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const HOST: &str = "device.tail123.ts.net";

/// A state directory, a Herdr, and a `tailscale` on the PATH that keeps its
/// Serve configuration in a file and is signed in as one owner.
struct Tailnet(PathBuf);

impl Tailnet {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("namedrop-tailnet-code-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        executable::write_executable(
            root.join("herdr"),
            "#!/bin/sh\nprintf '{\"result\":{\"workspace\":{\"label\":\"test\"}}}'\n",
        );
        executable::write_executable(
            root.join("tailscale"),
            include_str!("support/fake_tailscale.py"),
        );
        fs::write(
            root.join("in_terminal.py"),
            include_str!("support/in_terminal.py"),
        )
        .unwrap();
        for (name, contents) in [
            ("serve.json", json!({})),
            ("behavior.json", json!({})),
            (
                "identity.json",
                json!({"BackendState": "Running",
                       "Self": {"DNSName": format!("{HOST}."), "UserID": 42},
                       "User": {"42": {"LoginName": "owner@example.com"}}}),
            ),
        ] {
            fs::write(root.join(name), contents.to_string()).unwrap();
        }
        Self(root)
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    /// What `channels --no-open` prints for the tailnet link or the local
    /// one, in `format` (`--json`, `--human`, or the output's own default),
    /// to a terminal `columns` wide when there is one and to a pipe otherwise.
    /// `agent` is a variable an agent's session sets for the commands it runs.
    fn channels(
        &self,
        tailscale: bool,
        format: Option<&str>,
        columns: Option<u16>,
        agent: Option<&str>,
    ) -> String {
        let mut command = match columns {
            Some(columns) => {
                let mut command = Command::new("/usr/bin/python3");
                command
                    .arg(self.0.join("in_terminal.py"))
                    .arg(columns.to_string())
                    .arg(env!("CARGO_BIN_EXE_namedrop"));
                command
            }
            None => Command::new(env!("CARGO_BIN_EXE_namedrop")),
        };
        // The test itself may be run by an agent.
        for marker in namedrop::AGENT_SESSION_MARKERS {
            command.env_remove(marker);
        }
        command.envs(agent.map(|marker| (marker, "1")));
        let path = std::env::var("PATH").unwrap();
        let output = command
            .args(format)
            .arg("--plugin-state")
            .arg(self.state())
            .args(["--herdr-socket", "tailnet-test", "--herdr-workspace", "w1"])
            .args(["channels", "--no-open"])
            .args(tailscale.then_some("--tailscale"))
            .current_dir(&self.0)
            .env("PATH", format!("{}:{path}", self.0.display()))
            .env("TMPDIR", &self.0)
            .env("HOME", self.0.join("home"))
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("NAMEDROP_DASHBOARD_IDLE_MS", "10000")
            .output()
            .unwrap();
        printed(output)
    }
}

impl Drop for Tailnet {
    fn drop(&mut self) {
        let pattern = format!("{} dashboard-server", self.state().display());
        let _ = Command::new("pkill").args(["-f", &pattern]).status();
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

fn printed(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The tailnet link printed on its own line.
fn printed_link(text: &str) -> &str {
    text.lines()
        .filter_map(|line| line.strip_prefix("  "))
        .find(|link| link.starts_with(&format!("https://{HOST}/w/")))
        .unwrap_or_else(|| panic!("no link in {text:?}"))
}

fn draws_no_code(text: &str) -> bool {
    !text.contains(['▀', '▄', '█']) && !text.contains('\u{1b}')
}

fn assert_scans_as_the_link_under_it(shown: &str) {
    let link = printed_link(shown);
    for theme in [Theme::Dark, Theme::Light] {
        assert_eq!(
            scan(shown, theme).as_deref(),
            Some(link),
            "{theme:?}\n{shown}"
        );
    }
    assert!(
        shown.rfind('\u{1b}').unwrap() < shown.find(link).unwrap(),
        "the link belongs under the code:\n{shown}"
    );
}

#[test]
fn channels_prints_a_code_to_scan_for_a_person_and_the_same_json_for_a_program() {
    let tailnet = Tailnet::new();

    // Explicit JSON, and the default for output that is not a terminal.
    for format in [Some("--json"), None] {
        let text = tailnet.channels(true, format, None, None);
        assert!(
            text.is_ascii() && draws_no_code(&text),
            "{format:?}: {text:?}"
        );
        let mut result: Value = serde_json::from_str(&text).unwrap();
        let link = result["data"]["url"].take();
        assert!(
            link.as_str()
                .unwrap()
                .starts_with(&format!("https://{HOST}/w/")),
            "{link}"
        );
        result["data"]["server"].take();
        result["data"]["space_key"].take();
        assert_eq!(
            result,
            json!({
                "protocol_version": "collab/v1",
                "status": "ok",
                "operation": "dashboard",
                "data": {"url": null, "space_key": null, "workspace_id": "w1", "server": null,
                         "opened": false, "tailscale": true},
            }),
            "{format:?}"
        );
    }

    assert_scans_as_the_link_under_it(&tailnet.channels(true, None, Some(100), None));

    // Each of these readers gets the link and no code: a file or pipe asked
    // for readable text, a terminal that reports no width, a terminal too
    // narrow, and an agent whose shell is a pseudo-terminal.
    let link_only = [
        tailnet.channels(true, Some("--human"), None, None),
        tailnet.channels(true, None, Some(0), None),
        tailnet.channels(true, None, Some(40), None),
        tailnet.channels(true, None, Some(100), Some("CLAUDECODE")),
        tailnet.channels(true, None, Some(100), Some("CODEX_THREAD_ID")),
    ];
    for shown in &link_only {
        assert!(draws_no_code(shown), "{shown:?}");
        printed_link(shown);
    }
    let asks_for_width = |shown: &String| {
        shown.contains("This terminal is too narrow for a QR code to scan. Widen it to ")
    };
    assert_eq!(
        link_only.iter().map(asks_for_width).collect::<Vec<_>>(),
        [false, false, true, false, false]
    );

    let local = tailnet.channels(false, None, Some(100), None);
    assert!(
        draws_no_code(&local) && local.contains("  http://127.0.0.1:"),
        "{local:?}"
    );
}
