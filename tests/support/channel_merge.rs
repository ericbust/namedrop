use super::*;
use std::{path::PathBuf, sync::mpsc};

struct PromptGate(PathBuf);
impl Drop for PromptGate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn plain_channel_post_reaches_eof_before_prompt_drain() {
    let f = fixture();
    let server = Server::start(&f, r#"{"to":["planner"],"reason":"review request"}"#, false);
    let gate = PromptGate(f.root.join("prompt-gate"));
    fs::write(&gate.0, "").unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            sender
                .send(support::raw_post(
                    &server,
                    "merge-order",
                    "Could @planner review the parser?",
                    None,
                ))
                .unwrap();
        });
        let early = receiver.recv_timeout(Duration::from_secs(3));
        if early.is_err() {
            drop(gate);
            panic!("channel POST waited for prompt drain: {early:?}");
        }
        let (status, receipt) = early.unwrap();
        assert_eq!(status, 200, "{receipt}");
        assert!(receipt.get("drain").is_none(), "{receipt}");
        assert!(receipt.get("_channel_routing_note").is_none(), "{receipt}");
        assert_eq!(receipt["channel"]["state"], "routing");
        assert_eq!(receipt["channel"]["to"], json!([]));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !f.root.join("prompt-entered").exists() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(f.root.join("prompt-entered").exists());
        assert!(gate.0.exists());
        let row = history(&server)[0].clone();
        assert_eq!(row["channel"]["state"], "routed");
        assert_eq!(row["channel"]["to"], json!(["planner"]));
        drop(gate);
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while f.count(
        "SELECT COUNT(*) FROM delivery_attempt_state WHERE state IN ('submitted','acknowledged')",
    ) == 0
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    assert_eq!(
        f.count("SELECT COUNT(*) FROM delivery_attempt_state WHERE state IN ('submitted','acknowledged')"),
        1
    );
}

#[test]
fn channel_context_keeps_sidecar_metadata_from_history_and_frames() {
    for response in [
        r#"{"to":["planner"],"reason":"review request"}"#,
        "not json",
    ] {
        let f = fixture();
        let server = Server::start(&f, response, false);
        let (status, receipt) = post(
            &server,
            "merge-context",
            "Could @planner review this?",
            None,
        );
        assert_eq!(status, 200, "{receipt}");
        let id = receipt["message_id"].as_str().unwrap();
        let context = support::context(&server, id);
        let row = context["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["message_id"] == id)
            .unwrap();
        assert_eq!(row["channel"], receipt["channel"]);
        assert_eq!(history(&server)[0]["channel"], receipt["channel"]);
        let mut stream = events(&server);
        let frame = next_frame(&mut stream, &mut String::new());
        let row = frame["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["message_id"] == id)
            .unwrap();
        assert_eq!(row["channel"], receipt["channel"]);
    }
}
