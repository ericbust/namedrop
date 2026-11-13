//! What a dismiss by the person does to each kind of attention item, and
//! what it leaves alone.
use super::dashboard::ATTENTION_SHOWN_FOR_MS;
use super::*;
use crate::AttentionSourceKind::{AgentDisposition, Coordination, TerminalRelay};

fn fixture() -> (StateStore, StateNamespace) {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    initialize_schema(&mut connection, Path::new("unused")).unwrap();
    (
        StateStore {
            connection,
            state_directory: PathBuf::new(),
            read_only: false,
        },
        StateNamespace::for_space("test-socket", "test-dismissal"),
    )
}

fn person(at: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "local-human", at)
}

fn plugin(at: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "test-dismissal", at)
}

/// The ids of the cards Channels shows as needing the person at `now`, and
/// the space's health.
fn needs_you(store: &mut StateStore, ns: &StateNamespace, now: i64) -> (Vec<String>, String) {
    let rows = store.dashboard_rows(ns).unwrap();
    let wire = serde_json::to_value(crate::dashboard::snapshot::build(&rows, None, now)).unwrap();
    (
        wire["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["attention_id"].as_str().unwrap().to_owned())
            .collect(),
        wire["health"]["level"].as_str().unwrap().to_owned(),
    )
}

fn open(store: &mut StateStore, ns: &StateNamespace, id: &str, at: u64) {
    store
        .transaction(|tx| {
            tx.open_attention_item(
                ns,
                &AttentionItemDraft {
                    id: crate::AttentionItemId::new(id),
                    source_kind: Coordination,
                    source_id: "coordination".to_owned(),
                    summary: "This coordination needs a decision.".to_owned(),
                },
                &plugin(at),
            )
        })
        .unwrap();
}

fn dismiss(store: &mut StateStore, ns: &StateNamespace, id: &str, at: u64) -> bool {
    store
        .transaction(|tx| tx.dismiss_attention_item(ns, id, &person(at)))
        .unwrap()
}

fn open_kinds(store: &mut StateStore, ns: &StateNamespace) -> Vec<crate::AttentionSourceKind> {
    store
        .transaction(|tx| tx.unresolved_attention_items(ns))
        .unwrap()
        .into_iter()
        .map(|item| item.source_kind)
        .collect()
}

#[test]
fn a_terminal_reply_card_goes_when_dismissed_and_its_item_stays_for_a_reply_to_close() {
    let (mut store, ns) = fixture();
    seed_flagged_relay(&mut store, &ns, true, "Which option?");
    let (cards, health) = needs_you(&mut store, &ns, 30);
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(health, "needs_you");

    assert!(dismiss(&mut store, &ns, &cards[0], 31));
    let (after, health) = needs_you(&mut store, &ns, 32);
    assert!(after.is_empty(), "the card stayed: {after:?}");
    assert_ne!(health, "needs_you");
    assert!(
        open_kinds(&mut store, &ns).contains(&TerminalRelay),
        "the dismiss closed the item, so a reply has nothing to answer"
    );
    assert!(
        !dismiss(&mut store, &ns, &cards[0], 33),
        "a card already dismissed was dismissed again"
    );
}

#[test]
fn a_dismissed_request_warning_stays_dismissed_when_it_is_judged_again() {
    let (mut store, ns) = fixture();
    seed_unanswered(&mut store, &ns, false);
    let (cards, _) = needs_you(&mut store, &ns, 30);
    assert_eq!(cards, ["attention-agent-disposition-attempt"]);

    assert!(dismiss(&mut store, &ns, &cards[0], 31));
    assert!(needs_you(&mut store, &ns, 32).0.is_empty());
    assert_eq!(
        open_kinds(&mut store, &ns),
        [AgentDisposition],
        "the request is no longer owed, so its agent's answer would be dropped"
    );
    let dispositions: i64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM delivery_dispositions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(dispositions, 0, "a dismiss recorded a disposition");

    // A judgement withdraws a warning and raises it again when its reason changes.
    store
        .transaction(|tx| {
            tx.transaction.execute(
                "DELETE FROM attention_items WHERE attention_item_id = ?1",
                params![cards[0]],
            )?;
            tx.open_attention_item(
                &ns,
                &AttentionItemDraft {
                    id: crate::AttentionItemId::new(&cards[0]),
                    source_kind: AgentDisposition,
                    source_id: "attempt".to_owned(),
                    summary: "Its answer can't arrive.".to_owned(),
                },
                &plugin(40),
            )
        })
        .unwrap();
    assert!(
        needs_you(&mut store, &ns, 41).0.is_empty(),
        "a warning the person dismissed came back for the same request"
    );
    let doctor = store
        .transaction(|tx| tx.attention_items_needing_the_person(&ns))
        .unwrap();
    assert!(doctor.is_empty(), "Doctor still reports it: {doctor:?}");
}

#[test]
fn a_dismissed_card_returns_when_its_source_closes_it_and_raises_it_again() {
    let (mut store, ns) = fixture();
    seed_unanswered(&mut store, &ns, true);
    open(&mut store, &ns, "card", 20);
    assert_eq!(needs_you(&mut store, &ns, 21).0, ["card"]);
    assert!(dismiss(&mut store, &ns, "card", 22));
    assert!(needs_you(&mut store, &ns, 23).0.is_empty());

    open(&mut store, &ns, "card", 30);
    assert!(
        needs_you(&mut store, &ns, 31).0.is_empty(),
        "the source repeating itself brought the card back"
    );
    assert_eq!(open_kinds(&mut store, &ns), [Coordination]);

    store
        .transaction(|tx| {
            tx.resolve_attention_source(&ns, Coordination, "coordination", &plugin(40))
        })
        .unwrap();
    open(&mut store, &ns, "card", 50);
    assert_eq!(
        needs_you(&mut store, &ns, 51).0,
        ["card"],
        "a new warning from the same source stayed hidden"
    );
}

#[test]
fn a_stored_warning_is_shown_for_a_week() {
    let (mut store, ns) = fixture();
    seed_unanswered(&mut store, &ns, true);
    open(&mut store, &ns, "card", 1_000);
    let week = i64::try_from(ATTENTION_SHOWN_FOR_MS).unwrap();
    assert_eq!(needs_you(&mut store, &ns, 1_000 + week - 1).0, ["card"]);
    let (cards, health) = needs_you(&mut store, &ns, 1_000 + week);
    assert!(cards.is_empty(), "a week-old warning is still shown");
    assert_ne!(health, "needs_you");
    assert_eq!(
        open_kinds(&mut store, &ns),
        [Coordination],
        "the old warning was closed, not left for Doctor"
    );
}

#[test]
fn an_agent_waiting_in_its_pane_can_be_dismissed_until_it_waits_again() {
    let (mut store, ns) = fixture();
    seed_unanswered(&mut store, &ns, true);
    let address = store.dashboard_rows(&ns).unwrap().addresses[0]
        .address_id
        .clone();
    let waiting = |store: &mut StateStore, since: i64| {
        let mut rows = store.dashboard_rows(&ns).unwrap();
        let row = &mut rows.addresses[0];
        row.pane_type = crate::PaneType::Agent;
        row.retired = false;
        row.bound = true;
        row.occupied = true;
        row.agent_status = Some("blocked".to_owned());
        row.blocked_since_ms = Some(since);
        serde_json::to_value(crate::dashboard::snapshot::build(&rows, None, since + 5)).unwrap()
            ["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["attention_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let card = format!("agent-status-{address}");
    assert_eq!(waiting(&mut store, 100), std::slice::from_ref(&card));
    assert!(dismiss(&mut store, &ns, &card, 150));
    assert!(waiting(&mut store, 100).is_empty(), "the card stayed");
    assert_eq!(
        waiting(&mut store, 200),
        [card],
        "an agent that waits again shows no card"
    );
    assert!(!dismiss(&mut store, &ns, "agent-status-nobody", 201));
    assert!(!dismiss(&mut store, &ns, "no-such-card", 201));
}

#[test]
fn only_the_person_dismisses() {
    let (mut store, ns) = fixture();
    seed_unanswered(&mut store, &ns, true);
    open(&mut store, &ns, "card", 20);
    let refused = store.transaction(|tx| tx.dismiss_attention_item(&ns, "card", &plugin(21)));
    assert!(refused.is_err());
    assert_eq!(needs_you(&mut store, &ns, 22).0, ["card"]);
}
