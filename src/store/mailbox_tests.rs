use super::*;
use crate::{AgentSessionId, DeliveryAttemptId, DeliveryId, MessageRecipientDraft, PaneBindingId, PaneRoleId};

fn at(time: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "test", time)
}

fn setup(tx: &mut StateTransaction<'_>, ns: &StateNamespace) -> Result<(), StoreError> {
    tx.activate_worktree(ns, &at(1))?;
    tx.create_pane_address(ns, &PaneAddressDraft {
        id: PaneAddressId::new("address"), role_id: PaneRoleId::new("role"),
        role_name: "recipient".into(), role_description: "Recipient".into(), alias: "recipient".into(),
    }, &at(1))?;
    tx.bind_pane(ns, &PaneBindingId::new("binding"), &PaneAddressId::new("address"), "pane", &at(1))?;
    tx.observe_agent_session(ns, "observation", &PaneAddressId::new("address"), &AgentSessionId::new("session"), "idle", &at(1))?;
    tx.create_thread(ns, &ThreadId::new("thread"), "Mailbox", &at(1))?;
    tx.append_message(ns, &MessageDraft {
        thread_id: ThreadId::new("thread"), message_id: MessageId::new("message"), in_reply_to: None,
        author_address_id: PaneAddressId::new("address"), author_kind: ActorKind::Human,
        author_alias_snapshot: "recipient".into(), author_agent_session_snapshot: None,
        request_id: "request".into(), authored_source: "@recipient hello".into(), body: "hello".into(),
        grammar_version: "mention/v1".into(), context_references: vec![],
        recipients: vec![MessageRecipientDraft { address_id: PaneAddressId::new("address"), alias_snapshot: "recipient".into(), agent_session_snapshot: None, delivery_id: DeliveryId::new("delivery") }],
    }, &at(1))?;
    Ok(())
}

fn lease(id: &str) -> LeaseDraft {
    LeaseDraft { attempt_id: DeliveryAttemptId::new(id), binding_id: PaneBindingId::new("binding"),
        agent_session_id: AgentSessionId::new("session"), retry_of_attempt_id: None,
        owner: "test".into(), acquired_at_ms: 10, expires_at_ms: 1000 }
}

#[test]
fn mailbox_refusal_owns_its_pause_without_pausing_exception_prompts() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    store.transaction(|tx| {
        setup(tx, &ns)?;
        assert!(tx.lease_mailbox_head(&ns, &PaneAddressId::new("address"), &lease("attempt"), &at(10))?.is_some());
        tx.return_delivery_attempt_to_queue(&ns, &DeliveryAttemptId::new("attempt"), DeliveryReturnReason::DeferredUntilSettle { observed_status_ms: Some(100) }, &at(120))?;
        let facts = tx.transition_facts(&ns)?;
        assert!(!facts.iter().any(|fact| fact.entity_kind == TransitionEntityKind::PromptRecipient), "ordinary mailbox refusal wrote prompt_recipient facts");
        assert!(facts.iter().any(|fact| fact.entity_kind == TransitionEntityKind::MailboxDelivery && fact.entity_id == "delivery" && fact.transition_kind == TransitionKind::Paused));
        assert!(facts.iter().any(|fact| fact.entity_kind == TransitionEntityKind::MailboxDelivery && fact.entity_id == "delivery" && fact.transition_kind == TransitionKind::Observed && fact.occurred_at_ms == 100));
        assert_eq!(tx.delivery_attempt_state(&ns, &DeliveryAttemptId::new("attempt"))?, DeliveryState::Cancelled);
        assert!(tx.unresolved_attention_items(&ns)?.is_empty());
        assert!(!tx.prompt_is_deferred(&ns, "pane", &AgentSessionId::new("session"))?);
        Ok(())
    }).unwrap();
}

#[test]
fn binding_location_accepts_missing_tab_but_requires_pane() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    StateStore::open(&directory.0).unwrap().transaction(|tx| {
        setup(tx, &ns)?;
        tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "pane", "tab")?;
        tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "pane", "")?;
        assert_eq!(tx.active_pane_bindings(&ns)?[0].tab_id, None);
        assert!(tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "", "tab").is_err());
        Ok(())
    }).unwrap();
}

const GATE_QUERY: &str = "SELECT gate.transition_kind, COALESCE(observed.occurred_at_ms, 0)
FROM transition_facts gate LEFT JOIN transition_facts observed
ON observed.session_socket_id=gate.session_socket_id AND observed.worktree_id=gate.worktree_id
AND observed.worktree_order=gate.worktree_order-1 AND observed.entity_kind=gate.entity_kind
AND observed.entity_id=gate.entity_id AND observed.transition_kind='observed'
WHERE gate.session_socket_id=?1 AND gate.worktree_id=?2
AND gate.entity_kind='prompt_recipient' AND gate.entity_id=?3
AND gate.transition_kind IN ('paused','resumed') ORDER BY gate.worktree_order DESC LIMIT 1";

#[test]
fn recipient_gate_work_is_bounded_despite_unrelated_history() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    store.transaction(|tx| {
        tx.activate_worktree(&ns, &at(1))?;
        tx.defer_prompt_until_settle(&ns, "pane", &AgentSessionId::new("session"), Some(100), &at(120))?;
        tx.transaction.execute_batch("WITH RECURSIVE n(x) AS (VALUES(10) UNION ALL SELECT x+1 FROM n WHERE x<200009)
INSERT INTO transition_facts SELECT 'socket','space',x,'mailbox_delivery','unrelated-'||x,'queued','human','test',1 FROM n;")?;
        let mut stmt = tx.transaction.prepare(GATE_QUERY)?;
        let result: (String, i64) = stmt.query_row(params!["socket", "space", prompt_recipient_key("pane", &AgentSessionId::new("session"))], |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert_eq!(result, ("paused".into(), 100));
        let steps = stmt.get_status(rusqlite::StatementStatus::VmStep);
        assert!(steps < 1000, "recipient gate executed {steps} VM steps for one matching recipient");
        Ok(())
    }).unwrap();
}
