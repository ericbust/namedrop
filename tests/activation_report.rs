#![cfg(feature = "test-support")]
#[path = "support/binary.rs"]
mod binary;
use namedrop::{
    AgentConfigDirectories, Clock, IdGenerator, LifecycleRuntime, discover_worktree_for_workspace,
    execute_lifecycle_operation, mux::*, tour,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Host {
    inner: fake::FakeMux,
    popup: Result<Applied, MuxError>,
    reload: Applied,
}
impl MuxRead for Host {
    fn endpoint(&self) -> &Endpoint {
        self.inner.endpoint()
    }
    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        self.inner.survey(scope, depth)
    }
    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        self.inner.screen(pane, request)
    }
    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        self.inner.process(pane)
    }
    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        self.inner.tab_label(tab)
    }
    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        self.inner.integration_root()
    }
    fn within<'a>(&'a self, allowance: Allowance<'a>) -> Box<dyn MuxRead + 'a> {
        self.inner.within(allowance)
    }
}
impl MuxAct for Host {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        match effect {
            Effect::OpenPopup(_) => self.popup.clone(),
            Effect::ReloadKeybindings => Ok(self.reload.clone()),
            _ => self.inner.apply(effect),
        }
    }
    fn open_space(&self, cwd: &str, label: &str) -> Result<NewSpace, MuxError> {
        self.inner.open_space(cwd, label)
    }
    fn open_tab(
        &self,
        space: &namedrop::SpaceId,
        cwd: &str,
        label: &str,
    ) -> Result<NewTab, MuxError> {
        self.inner.open_tab(space, cwd, label)
    }
    fn close(&self, closing: &Closing) -> Result<(), MuxError> {
        self.inner.close(closing)
    }
}
impl MuxInput for Host {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        self.inner.deliver(input)
    }
}
struct Time;
impl Clock for Time {
    fn now_unix_ms(&self) -> u64 {
        100
    }
}
struct Ids(AtomicU64);
impl IdGenerator for Ids {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nd-activation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn host(endpoint: &str, popup: Result<Applied, MuxError>, reload: Applied) -> Host {
    Host {
        inner: fake::FakeMux::world()
            .endpoint(endpoint)
            .space("w1")
            .pane("p1")
            .build(),
        popup,
        reload,
    }
}
fn runtime<'a>(
    f: &'a Fixture,
    host: &'a Host,
    home: &'a Path,
    ids: &'a Ids,
) -> LifecycleRuntime<'a, Host, Time, Ids> {
    LifecycleRuntime {
        state_directory: &f.0,
        home_directory: Some(home),
        agent_config: AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: &host.endpoint().key,
        mux: host,
        clock: &Time,
        ids,
    }
}
#[test]
fn rex_activation_writes_no_herdr_keys_and_herdr_keeps_its_bytes() {
    for endpoint in ["rex:/tmp/fake.sock", "/tmp/fake.sock"] {
        let f = Fixture::new();
        let home = f.0.join("home");
        let ids = Ids(AtomicU64::new(0));
        let host = host(endpoint, Ok(Applied::Unsupported), Applied::Done);
        let rt = runtime(&f, &host, &home, &ids);
        let report =
            execute_lifecycle_operation("activate", json!({"confirmed":true}), &rt).unwrap();
        let config = home.join(".config/herdr/config.toml");
        if endpoint.starts_with("rex:") {
            assert_eq!(
                report["shortcuts"],
                json!({"status":"no_module","repair":"Run sh scripts/install.sh."})
            );
            assert!(!config.exists());
        } else {
            let expected = f.0.join("expected.toml");
            namedrop::install_shortcuts(Some(&expected), Some(&home)).unwrap();
            assert_eq!(fs::read(config).unwrap(), fs::read(expected).unwrap());
            assert_eq!(report["shortcuts"]["reloaded"], true);
        }
    }
}
#[test]
fn unsupported_reload_is_reported_as_an_error() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let ids = Ids(AtomicU64::new(0));
    let host = host(
        "/tmp/fake.sock",
        Ok(Applied::Unsupported),
        Applied::Unsupported,
    );
    let report = execute_lifecycle_operation(
        "activate",
        json!({"confirmed":true}),
        &runtime(&f, &host, &home, &ids),
    )
    .unwrap();
    assert_eq!(report["shortcuts"]["reloaded"], false);
    assert!(
        report["shortcuts"]["error"]
            .as_str()
            .unwrap()
            .contains("does not support")
    );
}
#[test]
fn only_an_unsupported_rex_popup_releases_the_tour_marker() {
    for endpoint in ["rex:/tmp/fake.sock", "/tmp/fake.sock"] {
        for popup in [
            Ok(Applied::Unsupported),
            Ok(Applied::Done),
            Err(MuxError::Unavailable("cannot open".into())),
        ] {
            let f = Fixture::new();
            let home = f.0.join("home");
            let ids = Ids(AtomicU64::new(0));
            let host = host(endpoint, popup.clone(), Applied::Done);
            let rt = runtime(&f, &host, &home, &ids);
            let mut space = discover_worktree_for_workspace(&host, "w1").unwrap();
            space.panes[0].agent = Some(AgentIdentity {
                kind: AgentKind::Known(namedrop::AgentRuntime::Codex),
                label: None,
                session: SessionClaim::None,
            });
            let first = tour::show_for_first_agent(&rt, &space, None);
            let second = tour::show_for_first_agent(&rt, &space, None);
            let retry = endpoint.starts_with("rex:") && popup == Ok(Applied::Unsupported);
            assert_eq!(tour::is_shown(&f.0, tour::CURRENT_ID).unwrap(), !retry);
            if retry {
                assert_eq!(first["reason"], "unsupported");
                assert_eq!(second["reason"], "unsupported");
            } else {
                assert_eq!(second["reason"], "already_shown");
            }
        }
    }
}
/// A detached Rex block has no window for the tour's layer. The tour is not
/// shown, and its marker stays unclaimed, until an agent is in a window, and
/// it prefers that agent over the detached one an event names.
#[test]
fn a_rex_tour_skips_detached_agents_and_leaves_its_marker_unclaimed() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let ids = Ids(AtomicU64::new(0));
    let rex = host(
        "rex:/tmp/fake.sock",
        Ok(Applied::Opened(json!({}))),
        Applied::Done,
    );
    let rt = runtime(&f, &rex, &home, &ids);
    let mut space = discover_worktree_for_workspace(&rex, "w1").unwrap();
    space.panes[0].agent = Some(AgentIdentity {
        kind: AgentKind::Known(namedrop::AgentRuntime::Codex),
        label: None,
        session: SessionClaim::None,
    });
    let attached = space.panes[0].clone();
    space.panes[0].tab_id.clear();
    let detached = space.panes[0].pane_id.clone();
    let report = tour::show_for_first_agent(&rt, &space, Some(&detached));
    assert_eq!(report["reason"], "no_window", "{report}");
    assert!(!tour::is_shown(&f.0, tour::CURRENT_ID).unwrap());
    space.panes.push(namedrop::DiscoveredPane {
        pane_id: "p2".into(),
        ..attached
    });
    let report = tour::show_for_first_agent(&rt, &space, Some(&detached));
    assert_eq!(report["shown"], true, "{report}");
    assert_eq!(report["pane_id"], "p2");

    // Herdr's tour is unchanged: it never reads the tab.
    let f = Fixture::new();
    let host = host(
        "/tmp/fake.sock",
        Ok(Applied::Opened(json!({}))),
        Applied::Done,
    );
    let rt = runtime(&f, &host, &home, &ids);
    space.panes.truncate(1);
    let report = tour::show_for_first_agent(&rt, &space, Some(&detached));
    assert_eq!(report["shown"], true, "{report}");
}
#[test]
fn rex_shortcut_repair_refuses_before_writing_any_config() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let config = f.0.join("override.toml");
    let out = binary::namedrop()
        .env("HOME", &home)
        .args([
            "--endpoint",
            "rex:/tmp/fake.sock",
            "--json",
            "ui",
            "install-shortcuts",
            "--config",
        ])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("namedrop.lua"),
        "{out:?}"
    );
    assert!(!config.exists());
    assert!(!home.join(".config/herdr/config.toml").exists());
}

#[test]
fn a_failed_rex_claim_cannot_install_herdr_shortcuts() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let config = f.0.join("override.toml");
    let out = binary::namedrop()
        .env("HOME", &home)
        .env("REX_BLOCK", "block:missing")
        .env("REX_SESSION", "session:missing")
        .env("REX_SERVER", "unix:///tmp/nd-ui-no-such-server.sock")
        .args(["--json", "ui", "install-shortcuts", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(!config.exists());
    assert!(!home.join(".config/herdr/config.toml").exists());
}

#[test]
fn rex_doctor_reports_a_module_left_behind_by_an_older_herdr_install() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let ids = Ids(AtomicU64::new(0));
    let host = host(
        "rex:/tmp/fake.sock",
        Ok(Applied::Unsupported),
        Applied::Done,
    );
    let rt = runtime(&f, &host, &home, &ids);
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &rt).unwrap();
    fs::write(
        home.join(".config/namedrop/rex.json"),
        json!({"state":"/old/standalone"}).to_string(),
    )
    .unwrap();
    let report = namedrop::diagnose_collaboration(&rt);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-INSTALL-003")
        .expect("module mismatch is reported");
    assert_eq!(
        issue.repair.prompt,
        "Update Namedrop in Herdr, then run Activate Namedrop there again."
    );
}

/// UI.md's "Rex first, then a Herdr release from before step 6": that
/// release rebakes the packages for its own state and writes no record, so
/// the record and the module still name the standalone state.
#[test]
fn rex_doctor_compares_the_packages_with_the_module_after_a_released_herdr_rebakes_them() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let ids = Ids(AtomicU64::new(0));
    let host = host(
        "rex:/tmp/fake.sock",
        Ok(Applied::Unsupported),
        Applied::Done,
    );
    let rt = runtime(&f, &host, &home, &ids);
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &rt).unwrap();
    let record = home.join(".config/namedrop/state-directory");
    let standalone = fs::read(&record).unwrap();
    fs::write(
        home.join(".config/namedrop/rex.json"),
        json!({"state": f.0}).to_string(),
    )
    .unwrap();
    let report = namedrop::diagnose_collaboration(&rt);
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.code == "ND-INSTALL-003"),
        "the packages and the module agree"
    );
    let herdr_state = f.0.join("herdr-state");
    fs::create_dir_all(&herdr_state).unwrap();
    let launcher = std::env::current_exe().unwrap();
    for agent in [
        namedrop::AgentRuntime::Codex,
        namedrop::AgentRuntime::ClaudeCode,
    ] {
        namedrop::install_integration_package(&home, agent, &launcher, &herdr_state).unwrap();
    }
    fs::write(&record, &standalone).unwrap();
    let report = namedrop::diagnose_collaboration(&rt);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-INSTALL-003")
        .expect("the packages bake another state than the module");
    assert_eq!(
        issue.repair.prompt,
        "Update Namedrop in Herdr, then run Activate Namedrop there again."
    );
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.code == "ND-INTEGRATION-002"),
        "activating again on Rex would take the packages back"
    );
}
