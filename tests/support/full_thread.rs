use super::*;
use namedrop::dashboard::snapshot::SpaceKey;

const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123";

fn body_path(message: &str) -> String {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    format!("/w/{}/messages/{message}/body", key.as_str())
}

fn long_source() -> String {
    let mut source = String::from("@planner ");
    while source.len() < 6_000 {
        source.push_str("lorem ipsum ");
    }
    source.push_str(&format!("token {SECRET} "));
    while source.chars().count() < 10_100 {
        source.push('x');
    }
    source.push_str(" THE END");
    source
}

#[test]
fn show_more_reads_the_full_redacted_body_behind_session_csrf_and_host() {
    let f = fixture();
    let server = Server::start(&f, "not json", false);
    let sent = f.agent_send("w1", "pane-1-lead", "long", &long_source());
    let id = sent["message_id"].as_str().unwrap();
    let row = history(&server)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["message_id"] == id)
        .cloned()
        .unwrap();
    assert_eq!(row["body"]["truncated"], true);
    assert_eq!(row["body"]["text"].as_str().unwrap().chars().count(), 2000);

    let csrf = server.csrf().to_owned();
    let full = server.request(
        "GET",
        &body_path(id),
        &[("Cookie", &server.cookie), ("X-Namedrop-Csrf", &csrf)],
        "",
    );
    assert_eq!(full.status, 200, "{}", full.body);
    let full: Value = serde_json::from_str(&full.body).unwrap();
    assert_eq!(full["message_id"], id);
    assert_eq!(full["body"]["truncated"], false);
    let text = full["body"]["text"].as_str().unwrap();
    assert!(text.ends_with(" THE END"), "{}", &text[text.len() - 40..]);
    assert!(text.chars().count() > 10_000);
    assert!(text.contains("[REDACTED TOKEN]"));
    assert!(!text.contains(SECRET));

    for (headers, status) in [
        (vec![("X-Namedrop-Csrf", csrf.as_str())], 401),
        (vec![("Cookie", server.cookie.as_str())], 403),
        (
            vec![
                ("Cookie", server.cookie.as_str()),
                ("X-Namedrop-Csrf", "0".repeat(64).leak() as &str),
            ],
            403,
        ),
        (
            vec![
                ("Cookie", server.cookie.as_str()),
                ("X-Namedrop-Csrf", csrf.as_str()),
                ("Host", "evil.test"),
            ],
            403,
        ),
        (
            vec![
                ("Cookie", server.cookie.as_str()),
                ("X-Namedrop-Csrf", csrf.as_str()),
                ("Sec-Fetch-Site", "cross-site"),
            ],
            403,
        ),
    ] {
        let refused = server.request("GET", &body_path(id), &headers, "");
        assert_eq!(refused.status, status, "{headers:?}: {}", refused.body);
        assert!(!refused.body.contains("THE END"));
    }
    let missing = server.request(
        "GET",
        &body_path("message-missing"),
        &[("Cookie", &server.cookie), ("X-Namedrop-Csrf", &csrf)],
        "",
    );
    assert_eq!(missing.status, 404, "{}", missing.body);
}

#[test]
fn full_body_is_bounded_by_the_operator_post_limit() {
    let f = fixture();
    let server = Server::start(&f, "not json", false);
    let source = format!("@planner {}", "y".repeat(80 * 1024));
    let sent = f.agent_send("w1", "pane-1-lead", "huge", &source);
    let csrf = server.csrf().to_owned();
    let full = server.request(
        "GET",
        &body_path(sent["message_id"].as_str().unwrap()),
        &[("Cookie", &server.cookie), ("X-Namedrop-Csrf", &csrf)],
        "",
    );
    assert_eq!(full.status, 200, "{}", full.body);
    let full: Value = serde_json::from_str(&full.body).unwrap();
    assert_eq!(full["body"]["truncated"], true);
    assert_eq!(
        full["body"]["text"].as_str().unwrap().chars().count(),
        64 * 1024
    );
}

fn set_grammar(f: &Fixture, message: &Value, grammar: &str) {
    f.db()
        .execute_batch(&format!(
            "DROP TRIGGER immutable_messages_update;
             UPDATE collaboration_messages SET grammar_version='{grammar}' WHERE message_id='{}';
             CREATE TRIGGER immutable_messages_update BEFORE UPDATE ON collaboration_messages BEGIN SELECT RAISE(ABORT, 'collaboration messages are immutable'); END;",
            message["message_id"].as_str().unwrap()
        ))
        .unwrap();
}

fn queued_for(f: &Fixture, receipt: &Value) -> Vec<String> {
    let db = f.db();
    let mut statement = db
        .prepare(
            "SELECT c.alias FROM mailbox_deliveries d JOIN alias_claims c USING (session_socket_id, worktree_id, address_id)
             WHERE d.message_id=?1 AND c.claim_state='active' ORDER BY c.alias",
        )
        .unwrap();
    statement
        .query_map([receipt["message_id"].as_str().unwrap()], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn reply_in_thread_from_every_row_kind_reaches_the_conversation_agents() {
    for grammar in [
        "mention/v1",
        "delegated/v1",
        "coordination-report/v1",
        "result-set/v1",
        "coordination-nudge/v1",
        "mention-hook/v1",
        "mention-return/v1",
        "terminal-result/v1",
    ] {
        let f = fixture();
        let server = Server::start(&f, "not json", false);
        let root = f.agent_send("w1", "pane-1-lead", "root", "@planner Please inspect.");
        set_grammar(&f, &root, grammar);
        let (status, reply) = post(
            &server,
            "reply",
            "Thanks, keep going.",
            root["message_id"].as_str(),
        );
        assert_eq!(status, 200, "{grammar}: {reply}");
        assert_eq!(recipient_aliases(&reply), ["lead", "planner"], "{grammar}");
        assert_eq!(queued_for(&f, &reply), ["lead", "planner"], "{grammar}");
        let stored: (String, Option<String>) = f
            .db()
            .query_row(
                "SELECT thread_id, in_reply_to FROM collaboration_messages WHERE message_id=?1",
                [reply["message_id"].as_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, root["thread_id"].as_str().unwrap(), "{grammar}");
        assert_eq!(
            stored.1.as_deref(),
            root["message_id"].as_str(),
            "{grammar}"
        );
    }
}

#[test]
fn reply_in_thread_to_agent_replies_and_operator_posts_reaches_the_agents() {
    let f = fixture();
    let server = Server::start(&f, "not json", false);
    let (status, root) = post(&server, "operator-root", "@planner Inspect.", None);
    assert_eq!(status, 200, "{root}");
    let (status, to_post) = post(
        &server,
        "to-post",
        "One more thing.",
        root["message_id"].as_str(),
    );
    assert_eq!(status, 200, "{to_post}");
    assert_eq!(queued_for(&f, &to_post), ["planner"]);
    let answer = agent_reply(&f, "answer", &root, "pane-1-planner", "@operator Found it.").unwrap();
    let (status, to_answer) = post(
        &server,
        "to-answer",
        "Great, fix it.",
        answer["message_id"].as_str(),
    );
    assert_eq!(status, 200, "{to_answer}");
    assert_eq!(queued_for(&f, &to_answer), ["planner"]);
}

#[test]
fn a_thread_rooted_on_a_coordination_report_with_no_agents_names_who_to_mention() {
    let f = fixture();
    let server = Server::start(&f, "not json", false);
    let (status, root) = post(&server, "statement", "A room statement.", None);
    assert_eq!(status, 200, "{root}");
    set_grammar(&f, &root, "coordination-report/v1");
    let (status, refused) = post(
        &server,
        "nobody",
        "Who reads this?",
        root["message_id"].as_str(),
    );
    assert_eq!(status, 400, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("Mention"),
        "{refused}"
    );
    let (status, mentioned) = post(
        &server,
        "mentioned",
        "@planner please pick this report up.",
        root["message_id"].as_str(),
    );
    assert_eq!(status, 200, "{mentioned}");
    assert_eq!(queued_for(&f, &mentioned), ["planner"]);
    let (status, next) = post(
        &server,
        "next",
        "And keep me posted.",
        mentioned["message_id"].as_str(),
    );
    assert_eq!(status, 200, "{next}");
    assert_eq!(queued_for(&f, &next), ["planner"]);
}
