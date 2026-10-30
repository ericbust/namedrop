use super::*;
use std::sync::mpsc;

#[test]
fn leading_unknown_aliases_are_refused_instead_of_silent_room_posts() {
    let f = fixture();
    let server = Server::start(&f, "not json", false);
    for (index, source) in ["@Planner inspect", "@planner_x inspect", "@Missing inspect"]
        .iter()
        .enumerate()
    {
        let (status, receipt) = post(&server, &format!("leading-unknown-{index}"), source, None);
        assert_eq!(
            status, 400,
            "leading unknown alias posted silently: {receipt}"
        );
    }
    assert_eq!(f.count("SELECT COUNT(*) FROM collaboration_messages"), 0);
    assert!(calls(&f).is_empty());
}

#[test]
fn post_is_visible_and_answers_before_router_finishes_then_survives_restart() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    let (sender, receiver) = mpsc::channel();
    let (early, visible) = thread::scope(|scope| {
        scope.spawn(|| {
            sender
                .send(post(
                    &server,
                    "store-first",
                    "Could @planner inspect this?",
                    None,
                ))
                .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while calls(&f).is_empty() && Instant::now() < deadline {
            thread::yield_now();
        }
        let early = receiver.recv_timeout(Duration::from_secs(2));
        let visible =
            f.count("SELECT COUNT(*) FROM collaboration_messages WHERE request_id='store-first'");
        fs::write(f.root.join("router-release"), "").unwrap();
        (early, visible)
    });
    assert_eq!(visible, 1, "post was not committed while router ran");
    let (status, receipt) = early.expect("POST waited for router result");
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["channel"]["state"], "routing");
    assert_eq!(receipt["recipients"], json!([]));
    let id = receipt["message_id"].as_str().unwrap();
    drop(server);
    let restarted = Server::start(&f, "not json", false);
    let row = history(&restarted)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["message_id"] == id)
        .cloned()
        .unwrap();
    assert_ne!(
        row["channel"]["state"], "routing",
        "restart left post permanently routing"
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM collaboration_messages WHERE request_id='store-first'"),
        1
    );
    let (_, replay) = post(
        &restarted,
        "store-first",
        "Could @planner inspect this?",
        None,
    );
    assert_eq!(replay["message_id"], receipt["message_id"]);
    assert_eq!(calls(&f).len(), 1);
}

#[test]
fn pulled_in_agent_answers_the_asker_without_repeating_its_name() {
    let f = fixture();
    let root = plain_root(&f, "asker-root", "Please collaborate here.");
    let question = agent_reply(
        &f,
        "asker-question",
        &root,
        "pane-1-planner",
        "@operator Asking @lead for the number.",
    )
    .unwrap();
    let answer = agent_reply(
        &f,
        "asker-answer",
        &question,
        "pane-1-lead",
        "@operator The number is 42.",
    )
    .unwrap();
    assert_eq!(recipient_aliases(&answer), vec!["operator", "planner"]);
    let envelope = f.herdr.last_envelope("pane-1-planner");
    assert_eq!(envelope["untrusted_body"], "The number is 42.");
    let mut store = StateStore::open(&f.state).unwrap();
    let controls = store
        .transaction(|tx| {
            tx.thread_control_state(
                &StateNamespace::for_space("socket-a", "w1"),
                &namedrop::ThreadId::new(root["thread_id"].as_str().unwrap()),
            )
        })
        .unwrap();
    assert_eq!(controls.reply_allowance, 8);
}
