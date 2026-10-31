#[path = "support/xws.rs"]
mod xws;

use namedrop::{
    ActorKind, AgentSessionId, DeliveryAttemptId, DeliveryState, LeaseDraft, MutationAttribution,
    PaneAddressId, PaneBindingId, StateNamespace, StateStore, StoreError,
};
use serde_json::Value;
use std::{fs, sync::atomic::Ordering};
use xws::Fixture;

fn records(f: &Fixture) -> Vec<Value> {
    fs::read_dir(f.root.join(".namedrop-diagnostic-evidence"))
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect()
}

#[test]
fn failed_writes_survive_rollback_requeue_and_purge() {
    let f = Fixture::new();
    f.register_all();
    f.status("pane-1-planner", "working");
    f.agent_send("w1", "pane-1-lead", "diagnostic", "@planner Investigate.");
    let ns = StateNamespace::for_space("socket-a", "w1");
    let at = MutationAttribution::new(ActorKind::Human, "diagnostic-test", 100);
    let mut store = StateStore::open(&f.state).unwrap();
    let lease = |tx: &mut namedrop::StateTransaction<'_>, id: &str| {
        tx.lease_mailbox_head(
            &ns,
            &PaneAddressId::new(xws::address_id("w1", "planner")),
            &LeaseDraft {
                attempt_id: DeliveryAttemptId::new(id),
                binding_id: PaneBindingId::new("binding-pane-1-planner"),
                agent_session_id: AgentSessionId::new(xws::session("pane-1-planner")),
                retry_of_attempt_id: None,
                owner: "diagnostic-test".into(),
                acquired_at_ms: 100,
                expires_at_ms: 1000,
            },
            &at,
        )
    };
    store.transaction(|tx| lease(tx, "first")).unwrap().unwrap();
    let rolled_back: Result<(), StoreError> = store.transaction(|tx| {
        tx.transition_delivery_attempt(
            &ns,
            &DeliveryAttemptId::new("first"),
            DeliveryState::Failed,
            &at,
        )?;
        Err(StoreError::invalid_input(
            "deliberate rollback after failed writes",
        ))
    });
    assert!(rolled_back.is_err());
    assert_eq!(
        store
            .transaction(|tx| tx.delivery_attempt_state(&ns, &DeliveryAttemptId::new("first")))
            .unwrap(),
        DeliveryState::Dispatching
    );
    let evidence = records(&f);
    assert_eq!(evidence.len(), 2);
    assert!(
        evidence
            .iter()
            .all(|r| r["event"]["phase"] == "sql_write_in_open_transaction")
    );
    assert!(
        evidence
            .iter()
            .any(|r| r["event"]["entity"] == "MailboxDelivery")
    );
    store
        .transaction(|tx| {
            tx.return_delivery_attempt_to_queue(&ns, &DeliveryAttemptId::new("first"), &at)
        })
        .unwrap();
    store
        .transaction(|tx| lease(tx, "second"))
        .unwrap()
        .unwrap();
    store
        .transaction(|tx| {
            tx.transition_delivery_attempt(
                &ns,
                &DeliveryAttemptId::new("second"),
                DeliveryState::Failed,
                &at,
            )
        })
        .unwrap();
    store
        .transaction(|tx| tx.deactivate_worktree(&ns, &at))
        .unwrap();
    store.purge_worktree(&ns, "w1", &at).unwrap();
    let evidence = records(&f);
    assert_eq!(evidence.len(), 5);
    assert!(
        evidence
            .iter()
            .any(|r| r["event"]["source"] == "return_delivery_attempt_to_queue")
    );
    assert_eq!(
        f.texts("SELECT attempt_id FROM delivery_attempts WHERE worktree_id='w1'"),
        Vec::<String>::new()
    );
}

fn deny_remote(f: &Fixture) {
    f.register_all();
    f.add_terminal("w2", "build", "pane-2-build");
    f.remote_control("address.allow-remote", "w2", "pane-2-lead", "build")
        .unwrap();
    f.herdr.shells_busy.store(true, Ordering::Relaxed);
    let sent = f.agent_send("w1", "pane-1-lead", "diagnostic-remote", "#ws2 @build make");
    assert_eq!(
        f.delivery_states("w2", &sent["forwarded"][0]["message_id"]),
        ["queued"]
    );
    f.remote_control("address.deny-remote", "w2", "pane-2-lead", "build")
        .unwrap();
    assert_eq!(
        f.delivery_states("w2", &sent["forwarded"][0]["message_id"]),
        ["failed"]
    );
    assert_eq!(f.delivery_states("w1", &sent["message_id"]), ["failed"]);
}

#[test]
fn mailbox_only_and_mirrored_failures_are_recorded() {
    let f = Fixture::new();
    deny_remote(&f);
    let evidence = records(&f);
    for source in ["deny_remote_commands", "end_envoy_delivery"] {
        assert!(
            evidence.iter().any(|r| r["event"]["source"] == source
                && r["event"]["entity"] == "MailboxDelivery"
                && r["event"]["phase"] == "sql_write_in_open_transaction"),
            "{evidence:?}"
        );
    }
}

#[test]
#[ignore = "intentional unrelated failure in subprocess"]
fn unrelated_failure_after_xws_drop() {
    {
        let f = Fixture::new();
        deny_remote(&f);
    }
    assert_eq!(
        std::process::id(),
        0,
        "unrelated assertion after xws fixture drop"
    );
}

#[test]
fn default_capture_contains_failed_events_after_xws_drop() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "unrelated_failure_after_xws_drop", "--ignored"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "unrelated assertion",
        "diagnostic-event",
        "deny_remote_commands",
        "end_envoy_delivery",
        "sql_write_in_open_transaction",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    println!("{stdout}");
}
