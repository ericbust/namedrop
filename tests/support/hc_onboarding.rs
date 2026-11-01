use super::*;
use crate::{HerdrCommandExecutor, HerdrCommandOutput, Host, IdGenerator};
use std::sync::Mutex;

#[allow(dead_code)]
#[path = "../fixtures/hc/process.rs"]
mod process;

struct FixtureRoot(std::path::PathBuf);
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Core {
    terminal: Mutex<serde_json::Value>,
    requests: Mutex<Vec<Vec<String>>>,
    fail_send: bool,
}
impl HerdrCommandExecutor for Core {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.requests
            .lock()
            .unwrap()
            .push(args.iter().map(|s| s.to_string()).collect());
        let mut terminal = self.terminal.lock().unwrap();
        let value = match args {
            ["ls", "--json"] => json!({"terminals":[*terminal]}),
            ["get", "t1", "--json"] => terminal.clone(),
            ["send", ..] if self.fail_send => return Err("permanent send failure".into()),
            ["send", ..] => json!({"sent":true}),
            ["label", "t1", labels @ ..] => {
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    terminal["labels"][key] = json!(value);
                }
                json!({})
            }
            other => panic!("unexpected request {other:?}"),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

fn exercise(mode: &str) {
    let ids = crate::ProcessIds::default();
    let root = FixtureRoot(std::env::temp_dir().join(ids.next_id("hc-onboarding")));
    let state = root.0.join("state");
    let home = root.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let agent = process::AgentProcess::with_program("claude");
    let pid = agent.pid();
    let mut terminal: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/hc/get-labeled.stdout")).unwrap();
    terminal["pid"] = json!(pid);
    terminal["foreground"] = json!({"pid":pid,"name":"claude","argv":["claude"]});
    terminal["labels"] = json!({"herdr.space":"fixture-space","namedrop.session":"session","namedrop.session.pid":pid.to_string(),"namedrop.session.start":process::start(pid)});
    terminal["status"] = json!({"app":"claude-code","state":"done"});
    terminal["status_ms"] = json!(1);
    let core = Core {
        terminal: Mutex::new(terminal),
        requests: Mutex::new(vec![]),
        fail_send: mode == "failure",
    };
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:onboarding",
        host: crate::HcHost::new(&core),
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let observed = runtime.host.space(runtime.space_locator(), None).unwrap();
    let namespace = observed.namespace(runtime.session_socket_id);
    let attribution = MutationAttribution::new(crate::ActorKind::Human, "test", 1);
    let address = PaneAddressId::new("address");
    let session = AgentSessionId::new(if mode == "mismatch" {
        "other"
    } else {
        "session"
    });
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &attribution)?;
            tx.create_pane_address(
                &namespace,
                &crate::PaneAddressDraft {
                    id: address.clone(),
                    role_id: crate::PaneRoleId::new("role"),
                    role_name: "Worker".into(),
                    role_description: if matches!(
                        mode,
                        "automatic" | "automatic_session_changed" | "registered"
                    ) {
                        "Native collaborator addressed as @worker"
                    } else {
                        "Manual worker"
                    }
                    .into(),
                    alias: "worker".into(),
                },
                &attribution,
            )?;
            tx.bind_pane(
                &namespace,
                &crate::PaneBindingId::new("binding"),
                &address,
                "t1",
                &attribution,
            )?;
            tx.observe_agent_session(
                &namespace,
                "observation",
                &address,
                &session,
                "done",
                &attribution,
            )?;
            Ok(())
        })
        .unwrap();
    crate::refresh_user_integration_packages(&runtime, &observed).unwrap();
    if mode.ends_with("session_changed") {
        core.terminal.lock().unwrap()["labels"]["namedrop.session"] = json!("session-B");
    }
    if mode == "working" {
        core.terminal.lock().unwrap()["status"]["state"] = json!("working");
    }
    if mode == "dead" {
        drop(agent);
    }
    if mode == "invoked" {
        onboard_settled_integrations(&runtime, &observed).unwrap();
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                tx.mark_integration_challenge_invoked(&namespace, &address, &session, &attribution)
            })
            .unwrap();
    }
    if mode == "registered" {
        onboard_settled_integrations(&runtime, &observed).unwrap();
        let registration = StateStore::open(&state)
            .unwrap()
            .transaction(|tx| tx.active_integration_registration(&namespace, &address, &session))
            .unwrap();
        assert!(registration.is_some(), "fixture must start registered");
    }
    core.requests.lock().unwrap().clear();
    let result = if mode == "public" {
        issue_registration_challenge(&address, AgentRuntime::ClaudeCode, &runtime).map(|_| ())
    } else {
        onboard_settled_integrations(&runtime, &observed).map(|_| ())
    };
    if matches!(mode, "mismatch" | "dead") {
        assert!(result.is_err());
    } else if mode != "dead" && !mode.ends_with("session_changed") {
        result.as_ref().unwrap();
    }
    let requests = core.requests.lock().unwrap();
    let sends = requests.iter().filter(|args| args[0] == "send").count();
    assert_eq!(
        sends,
        usize::from(matches!(mode, "manual" | "failure")),
        "{mode}: {requests:?}"
    );
    if mode.ends_with("session_changed") {
        let registration = StateStore::open(&state)
            .unwrap()
            .transaction(|tx| tx.active_integration_registration(&namespace, &address, &session))
            .unwrap();
        assert!(
            registration.is_none(),
            "stale session A was registered while core owns session B: {registration:?}"
        );
        assert!(result.is_err(), "stale session must be rejected");
    }
    let validations = match mode {
        "registered" => 0,
        "manual" | "working" | "invoked" => 2,
        "failure" => 3,
        "automatic"
        | "public"
        | "mismatch"
        | "dead"
        | "manual_session_changed"
        | "automatic_session_changed" => 1,
        other => panic!("unknown onboarding mode {other}"),
    };
    let lists = requests.iter().filter(|args| args[0] == "ls").count();
    let gets = requests.iter().filter(|args| args[0] == "get").count();
    eprintln!("onboarding {mode}: LS={lists} GET={gets} SEND={sends} result={result:?}");
    // Each validation discovers the single live terminal. Host::prompt adds a targeted get.
    assert_eq!(lists, validations, "{mode}: {requests:?}");
    assert_eq!(gets, validations + sends, "{mode}: {requests:?}");
    let registration = StateStore::open(&state)
        .unwrap()
        .transaction(|tx| tx.active_integration_registration(&namespace, &address, &session))
        .unwrap();
    assert_eq!(
        registration.is_some(),
        matches!(mode, "automatic" | "registered")
    );
    let (attention, deferred) = StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            Ok((
                tx.unresolved_attention_items(&namespace)?,
                tx.prompt_is_deferred(&namespace, "t1", &session)?,
            ))
        })
        .unwrap();
    assert_eq!(
        attention.len(),
        usize::from(matches!(mode, "failure" | "invoked")),
        "{mode}: {attention:?}"
    );
    assert!(!deferred, "{mode}: pre-prompt validation does not defer");
}

#[test]
fn manual_challenge_bounds_fresh_validation_cost() {
    exercise("manual");
}
#[test]
fn automatic_registration_bounds_fresh_validation_cost() {
    exercise("automatic");
}
#[test]
fn failed_prompt_bounds_fresh_validation_cost() {
    exercise("failure");
}
#[test]
fn snapshot_session_must_match_address() {
    exercise("mismatch");
}
#[test]
fn working_recipient_is_not_prompted() {
    exercise("working");
}
#[test]
fn dead_owner_is_not_prompted() {
    exercise("dead");
}

#[test]
fn invoked_challenge_failure_bounds_fresh_validation_cost() {
    exercise("invoked");
}
#[test]
fn public_challenge_still_discovers_fresh() {
    exercise("public");
}

#[test]
fn manual_session_changed_before_batch_is_not_prompted() {
    exercise("manual_session_changed");
}
#[test]
fn automatic_session_changed_before_batch_is_not_registered() {
    exercise("automatic_session_changed");
}

#[test]
fn registered_settled_batch_needs_no_validation() {
    exercise("registered");
}
