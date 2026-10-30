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
fn capitalized_leading_aliases_report_the_original_unknown_mention() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    for (index, alias) in [
        "Nobody",
        "Codex",
        "Planner",
        "PLANNER",
        "Mixed-Case",
        "nobody",
    ]
    .iter()
    .enumerate()
    {
        let (status, receipt) = post(
            &server,
            &format!("capitalized-unknown-{index}"),
            &format!("@{alias} inspect"),
            None,
        );
        assert_eq!(status, 400, "{receipt}");
        assert_eq!(receipt["error"], format!("unknown Mention: @{alias}"));
    }
    for (index, source) in [
        "@planner_x inspect",
        "@Planner! inspect",
        "@-Planner inspect",
    ]
    .iter()
    .enumerate()
    {
        let (status, receipt) = post(&server, &format!("malformed-leading-{index}"), source, None);
        assert_eq!(status, 400, "{receipt}");
        assert_eq!(
            receipt["error"],
            "message must begin with one or more recipient Mentions"
        );
    }
    let (status, mixed) = post(&server, "mixed-leading", "@planner @Codex inspect", None);
    assert_eq!(status, 400, "{mixed}");
    assert_eq!(mixed["error"], "unknown Mention: @Codex");
    for (index, source) in ["#ws1 @Codex inspect", "#ws1 @planner @Codex inspect"]
        .iter()
        .enumerate()
    {
        let (status, scoped) = post(&server, &format!("scoped-leading-{index}"), source, None);
        assert_eq!(status, 400, "{scoped}");
        assert_eq!(scoped["error"], "unknown Mention: @Codex");
    }
    assert_eq!(f.count("SELECT COUNT(*) FROM collaboration_messages"), 0);
    assert_eq!(f.count("SELECT COUNT(*) FROM mailbox_deliveries"), 0);
    for (index, source) in [
        "@planner Inspect @Codex",
        "@planner @Codex! stays body text.",
    ]
    .iter()
    .enumerate()
    {
        let (status, receipt) = post(&server, &format!("capitalized-body-{index}"), source, None);
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(recipient_aliases(&receipt), ["planner"]);
        assert_eq!(receipt["context_references"], json!([]));
    }
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
                .send(support::raw_post(
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
        if early.is_err() {
            fs::write(f.root.join("router-release"), "").unwrap();
        }
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
    fs::write(f.root.join("router-release"), "").unwrap();
    assert_eq!(
        row["channel"]["state"], "router_failed",
        "restart must fail the interrupted route"
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
    for report_source in [
        "@operator The number is 42.",
        "@operator The answer from \\@lead is 42.",
    ] {
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
        let report =
            agent_reply(&f, "asker-report", &answer, "pane-1-planner", report_source).unwrap();
        assert_eq!(recipient_aliases(&report), vec!["operator"]);
        assert_eq!(f.count("SELECT COUNT(*) FROM delivery_attempts"), 3);
        assert_eq!(f.herdr.envelopes("pane-1-lead").len(), 1);
    }
}

#[test]
fn leading_at_sign_followed_by_whitespace_is_plain_channel_text() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    for (index, source) in ["@ room statement", "@\troom statement", "@\nroom statement"]
        .iter()
        .enumerate()
    {
        let (status, receipt) = post(&server, &format!("leading-at-{index}"), source, None);
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(receipt["channel"]["state"], "unrouted");
        assert_eq!(receipt["recipients"], json!([]));
    }
    assert!(calls(&f).is_empty());
}

#[test]
fn explicit_followup_still_wakes_the_answerer_when_the_answer_mentions_the_asker() {
    let f = fixture();
    let root = plain_root(&f, "followup-root", "Please collaborate here.");
    let question = agent_reply(
        &f,
        "followup-question",
        &root,
        "pane-1-planner",
        "@operator Asking @lead for the number.",
    )
    .unwrap();
    let answer = agent_reply(
        &f,
        "followup-answer",
        &question,
        "pane-1-lead",
        "@operator The number for @planner is 42.",
    )
    .unwrap();
    assert_eq!(recipient_aliases(&answer), vec!["operator", "planner"]);
    let followup = agent_reply(
        &f,
        "followup-new-question",
        &answer,
        "pane-1-planner",
        "@operator Can @lead explain the calculation?",
    )
    .unwrap();
    assert_eq!(recipient_aliases(&followup), vec!["operator", "lead"]);
    assert_eq!(f.count("SELECT COUNT(*) FROM delivery_attempts"), 4);
}

#[test]
fn renamed_answerer_keeps_the_stored_ask_identity() {
    let f = fixture();
    let root = plain_root(&f, "rename-root", "Please collaborate here.");
    let question = agent_reply(
        &f,
        "rename-question",
        &root,
        "pane-1-planner",
        "@operator Asking @lead for the number.",
    )
    .unwrap();
    f.rename("w1", "lead", "renamed");
    let answer = agent_reply(
        &f,
        "rename-answer",
        &question,
        "pane-1-lead",
        "@operator The number is 42.",
    )
    .unwrap();
    assert_eq!(recipient_aliases(&answer), vec!["operator", "planner"]);
}

#[test]
fn manually_delivered_router_followup_under_a_mention_root_keeps_agent_thread_mode() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let (status, root) = post(&server, "s4-reply-root", "@planner Inspect.", None);
    assert_eq!(status, 200, "{root}");
    let (status, followup) = post(
        &server,
        "s4-reply-followup",
        "Please ask @lead to review.",
        root["message_id"].as_str(),
    );
    assert_eq!(status, 200, "{followup}");
    assert_eq!(followup["channel"]["state"], "unrouted");
    let key =
        namedrop::dashboard::snapshot::SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let response = server.post(
        &format!(
            "/w/{}/messages/{}/deliver",
            key.as_str(),
            followup["message_id"].as_str().unwrap()
        ),
        r#"{"to":["lead"]}"#,
        &[],
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let answer = agent_reply(
        &f,
        "s4-reply-answer",
        &followup,
        "pane-1-lead",
        "@operator The result mentions @planner.",
    )
    .unwrap();
    assert_eq!(recipient_aliases(&answer), vec!["operator"]);
    let invocations = fs::read_to_string(f.root.join("http-herdr-invocations.jsonl")).unwrap();
    let envelope = invocations
        .lines()
        .find_map(|line| {
            let invocation: Value = serde_json::from_str(line).unwrap();
            let args = invocation["args"].as_array()?;
            if args[0] != "agent" || args[1] != "prompt" || args[2] != "pane-1-lead" {
                return None;
            }
            let prompt: Value = serde_json::from_str(args[3].as_str()?).ok()?;
            Some(prompt)
        })
        .expect("manual delivery envelope for lead");
    assert_eq!(envelope["control"]["reply_mode"], "agent_thread");
}
