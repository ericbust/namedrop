use super::*;
use namedrop::dashboard::snapshot::SpaceKey;

fn path(workspace: &str, message: &Value) -> String {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", workspace));
    format!(
        "/w/{}/messages/{}/deliver",
        key.as_str(),
        message["message_id"].as_str().unwrap()
    )
}

fn deliver(server: &Server, message: &Value, aliases: &[&str]) -> (u16, Value) {
    let response = server.post(
        &path("w1", message),
        &json!({"to":aliases}).to_string(),
        &[],
    );
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

#[test]
fn stored_delivery_updates_original_post_without_copying_or_rerouting() {
    let f = fixture();
    let server = Server::start(&f, r#"{"to":[],"reason":"credit only"}"#, false);
    let source = "Please ask @planner to inspect verbatim.";
    let (_, root) = post(&server, "manual-root", source, None);
    let mut stream = events(&server);
    let mut buffered = String::new();
    let initial = next_frame(&mut stream, &mut buffered);
    assert_eq!(initial["messages"].as_array().unwrap().len(), 1);
    let notes = fs::read(f.state.join("dashboard/channel-routing.jsonl")).unwrap();
    let before: (String,String,String,String) = f.db().query_row("SELECT m.message_id,m.authored_source,m.body,i.request_fingerprint FROM collaboration_messages m JOIN idempotency_records i USING(session_socket_id,worktree_id,message_id) WHERE m.request_id='manual-root'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    let (status, receipt) = deliver(&server, &root, &["planner"]);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["message_id"], root["message_id"]);
    assert_eq!(receipt["channel"]["state"], "unrouted");
    assert_eq!(receipt["channel"]["to"], json!(["planner"]));
    assert_eq!(receipt["channel"]["reason"], "credit only");
    let changed = next_frame(&mut stream, &mut buffered);
    assert_eq!(changed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(changed["messages"][0]["message_id"], root["message_id"]);
    assert_eq!(changed["messages"][0]["channel"], receipt["channel"]);
    assert_eq!(changed["messages"][0]["legs"].as_array().unwrap().len(), 1);
    let (_, replay) = deliver(&server, &root, &["planner", "planner"]);
    assert_eq!(replay, receipt);
    assert_eq!(f.count("SELECT count(*) FROM collaboration_messages"), 1);
    assert_eq!(f.count("SELECT count(*) FROM message_recipients"), 1);
    assert_eq!(f.count("SELECT count(*) FROM mailbox_deliveries"), 1);
    let after: (String,String,String,String) = f.db().query_row("SELECT m.message_id,m.authored_source,m.body,i.request_fingerprint FROM collaboration_messages m JOIN idempotency_records i USING(session_socket_id,worktree_id,message_id) WHERE m.request_id='manual-root'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(before, after);
    assert_eq!(
        fs::read(f.state.join("dashboard/channel-routing.jsonl")).unwrap(),
        notes
    );
    assert_eq!(calls(&f).len(), 1);
}
