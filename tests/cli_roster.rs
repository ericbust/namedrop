#[path = "support/environment.rs"]
mod environment;

use environment::CommandHome;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    process::Command,
};

fn run_core(home: &Path, response: Value) -> (std::process::Output, Value) {
    let socket = home.join("core.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = std::thread::spawn(move || {
        let stream = (0..100).find_map(|_| match listener.accept() {
            Ok((stream, _)) => Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                None
            }
            Err(error) => panic!("fake core accept: {error}"),
        });
        let Some(stream) = stream else {
            return Value::Null;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let mut stream = reader.into_inner();
        writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":request["id"],"result":response})
        )
        .unwrap();
        request
    });
    let output = run(home, &socket, "hc");
    (output, worker.join().unwrap())
}

fn seed_address(state: &Path, core: &str, space: &str, alias: &str, session: Option<&str>) {
    namedrop::StateStore::open(state).unwrap();
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO worktrees VALUES (?1,?2,1,0,0,1)",
            (core, space),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO pane_roles VALUES (?1,?2,'role','agent','')",
            (core, space),
        )
        .unwrap();
    connection.execute("INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES (?1,?2,'address','role',0,0,1)", (core, space)).unwrap();
    connection
        .execute(
            "INSERT INTO alias_claims VALUES (?1,?2,?3,'address','active',NULL)",
            (core, space, alias),
        )
        .unwrap();
    if let Some(session) = session {
        connection
            .execute(
                "INSERT INTO pane_bindings VALUES (?1,?2,'binding','address','%1',1,NULL,1,NULL)",
                (core, space),
            )
            .unwrap();
        connection.execute("INSERT INTO agent_session_observations VALUES (?1,?2,'observation','address',?3,'working',2)", (core, space, session)).unwrap();
    }
}

fn metadata(state: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut entries = std::fs::read_dir(state)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                meta.len(),
                meta.modified().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

struct PrivateHome(std::path::PathBuf);

impl PrivateHome {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "namedrop-roster-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateHome {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(home: &Path, socket: &Path, host: &str) -> std::process::Output {
    let hc = home.join("hc-fixture");
    std::fs::write(
        &hc,
        "#!/bin/sh\nprintf called >> \"$HOME/hc.log\"\nexit 97\n",
    )
    .unwrap();
    std::fs::set_permissions(&hc, std::fs::Permissions::from_mode(0o700)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_home(home)
        .env("HC_SOCKET", socket)
        .env("HC_BIN_PATH", hc)
        .env("NAMEDROP_STATE_DIR", home.join("state"))
        .args(["--host", host, "roster", "--json"])
        .output()
        .unwrap()
}

#[test]
fn absent_core_returns_empty_direct_snapshot_without_creating_state() {
    let home = PrivateHome::new();
    let socket = home.path().join("absent.sock");
    let output = run(home.path(), &socket, "hc");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":null,"rows":[],"state_directory":home.path().join("state")})
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn stopped_core_returns_empty_snapshot() {
    let home = PrivateHome::new();
    let socket = home.path().join("stale.sock");
    drop(UnixListener::bind(&socket).unwrap());
    let output = run(home.path(), &socket, "hc");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["core"],
        Value::Null
    );
}

#[test]
fn running_core_uses_one_status_rpc_and_keeps_state_absent() {
    let home = PrivateHome::new();
    let (output, request) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(request["method"], "server.status");
    assert!(!home.path().join("hc.log").exists());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":"current","rows":[],"state_directory":home.path().join("state")})
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn seeded_unbound_address_is_nullable_and_state_is_unchanged() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:current", "space", "alpha", None);
    let before = metadata(&state);
    let (output, request) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(request["method"], "server.status");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":"current","rows":[{"alias":"alpha","terminal":null,"space":"space","lifecycle":"unbound","agent_status":null,"waiting_on":[],"attention":0,"recent_replies":[]}],"state_directory":state})
    );
    assert_eq!(metadata(&state), before);
}

#[test]
fn malformed_core_response_is_an_error() {
    let home = PrivateHome::new();
    let (output, _) = run_core(home.path(), json!({"core":{}}));
    assert_eq!(output.status.code(), Some(65), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["code"], "operation_failed");
}

#[test]
fn rows_only_include_the_current_core() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:other", "space", "foreign", None);
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["rows"], json!([]));
}

#[test]
fn seeded_wait_reply_and_attention_have_exact_json() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:current", "space", "alpha", Some("session"));
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO collaboration_threads VALUES ('hc:current','space','thread','',1);
         INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','request','thread',1,NULL,'address','alpha','agent_session','session','req','test','private body','mention/v1',2);
         INSERT INTO message_recipients VALUES ('hc:current','space','request',1,'address','alpha','session');
         INSERT INTO mailbox_deliveries VALUES ('hc:current','space','delivery','address',1,'request','queued',2);
         INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','reply','thread',2,'request','address','beta','agent_session','session','reply-req','test','private reply','mention/v1',4);
         INSERT INTO message_recipients VALUES ('hc:current','space','reply',1,'address','alpha','session');
         INSERT INTO mailbox_deliveries VALUES ('hc:current','space','return','address',2,'reply','settled',4);
         INSERT INTO attention_items VALUES ('hc:current','space','attention','delivery','delivery','private summary',3,NULL);",
    ).unwrap();
    let before = metadata(&state);
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":"current","rows":[{"alias":"alpha","terminal":"%1","space":"space","lifecycle":"occupied","agent_status":"working","waiting_on":[{"alias":"alpha","message_id":"request"}],"attention":1,"recent_replies":[{"message_id":"reply","from":"beta","at_ms":4}]}],"state_directory":state})
    );
    assert_eq!(metadata(&state), before);
}

#[test]
fn waits_and_replies_are_bounded_and_keep_newest_first() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:current", "space", "alpha", Some("session"));
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection
        .execute_batch(
            "INSERT INTO collaboration_threads VALUES ('hc:current','space','thread','',1)",
        )
        .unwrap();
    for index in 0..52 {
        let id = format!("request-{index:03}");
        connection.execute(
            "INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms) VALUES ('hc:current','space',?1,'thread',?2,NULL,'address','alpha','agent_session','session',?1,'test','private','mention/v1',?2)",
            rusqlite::params![id, index + 1],
        ).unwrap();
        connection.execute("INSERT INTO message_recipients VALUES ('hc:current','space',?1,1,'address','alpha','session')", [&id]).unwrap();
        connection.execute("INSERT INTO mailbox_deliveries VALUES ('hc:current','space',?1,'address',?2,?3,'queued',?2)", rusqlite::params![format!("delivery-{index:03}"), index + 1, id]).unwrap();
    }
    for index in 0..6 {
        let id = format!("reply-{index:03}");
        connection.execute(
            "INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms) VALUES ('hc:current','space',?1,'thread',?2,'request-000','address','beta','agent_session','session',?1,'test','private','mention/v1',?2)",
            rusqlite::params![id, index + 53],
        ).unwrap();
        connection.execute("INSERT INTO message_recipients VALUES ('hc:current','space',?1,1,'address','alpha','session')", [&id]).unwrap();
        connection.execute("INSERT INTO mailbox_deliveries VALUES ('hc:current','space',?1,'address',?2,?3,'settled',?2)", rusqlite::params![format!("return-{index:03}"), index + 53, id]).unwrap();
    }
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let waits = body["rows"][0]["waiting_on"].as_array().unwrap();
    assert_eq!(waits.len(), 50);
    assert_eq!(waits[0]["message_id"], "request-051");
    assert_eq!(waits[49]["message_id"], "request-002");
    let replies = body["rows"][0]["recent_replies"].as_array().unwrap();
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["message_id"], "reply-005");
    assert_eq!(replies[4]["message_id"], "reply-001");
}

#[test]
fn tui_nested_roster_help_is_not_exposed() {
    let home = PrivateHome::new();
    for args in [vec!["roster", "--help"], vec!["help", "roster"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .isolated_home(home.path())
            .env("HC_SOCKET", home.path().join("absent.sock"))
            .args(args)
            .output()
            .unwrap();
        assert_ne!(output.status.code(), Some(0), "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains("Read the current hc collaboration roster")
        );
    }
}

#[test]
fn one_request_with_many_recipients_emits_only_fifty_stable_waits() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:current", "space", "alpha", Some("session"));
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO collaboration_threads VALUES ('hc:current','space','thread','',1);
         INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','request','thread',1,NULL,'address','alpha','agent_session','session','req','test','private','mention/v1',2);",
    ).unwrap();
    for index in (0..52).rev() {
        let address = format!("recipient-{index:03}");
        let alias = format!("agent-{index:03}");
        connection.execute("INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('hc:current','space',?1,'role',0,0,1)", [&address]).unwrap();
        connection.execute("INSERT INTO message_recipients VALUES ('hc:current','space','request',?1,?2,?3,NULL)", rusqlite::params![53-index, address, alias]).unwrap();
        connection.execute("INSERT INTO mailbox_deliveries VALUES ('hc:current','space',?1,?2,?3,'request','queued',2)", rusqlite::params![format!("delivery-{index:03}"), address, index+1]).unwrap();
    }
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let waits = body["rows"][0]["waiting_on"].as_array().unwrap();
    assert_eq!(waits.len(), 50);
    assert_eq!(
        waits[0],
        json!({"alias":"agent-000","message_id":"request"})
    );
    assert_eq!(
        waits[49],
        json!({"alias":"agent-049","message_id":"request"})
    );
}

#[test]
fn previous_session_messages_do_not_enter_current_row() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(
        &state,
        "hc:current",
        "space",
        "alpha",
        Some("current-session"),
    );
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO collaboration_threads VALUES ('hc:current','space','thread','',1);
         INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','old-request','thread',1,NULL,'address','alpha','agent_session','old-session','old','test','private','mention/v1',2);
         INSERT INTO message_recipients VALUES ('hc:current','space','old-request',1,'address','alpha','old-session');
         INSERT INTO mailbox_deliveries VALUES ('hc:current','space','old-delivery','address',1,'old-request','queued',2);
         INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','old-reply','thread',2,'old-request','address','beta','agent_session','old-session','old-reply-req','test','private','mention/v1',3);
         INSERT INTO message_recipients VALUES ('hc:current','space','old-reply',1,'address','alpha','old-session');
         INSERT INTO mailbox_deliveries VALUES ('hc:current','space','old-return','address',2,'old-reply','settled',3);",
    ).unwrap();
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["rows"][0]["waiting_on"], json!([]));
    assert_eq!(body["rows"][0]["recent_replies"], json!([]));
}

#[test]
fn unbound_waits_use_latest_attempt_and_keep_attention_recipients() {
    let home = PrivateHome::new();
    let state = home.path().join("state");
    seed_address(&state, "hc:current", "space", "alpha", None);
    let connection = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms)
           VALUES ('hc:current','space','other','role',0,0,1);
         INSERT INTO pane_bindings VALUES ('hc:current','space','binding','address','%1',1,NULL,1,NULL);
         INSERT INTO collaboration_threads VALUES ('hc:current','space','thread','',1);",
    ).unwrap();
    for (index, state, disposition) in [
        (1, "held", "reply"),
        (2, "failed", "no_reply"),
        (3, "queued", "reply"),
    ] {
        let message = format!("request-{index}");
        let delivery = format!("delivery-{index}");
        connection.execute(
            "INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
               VALUES ('hc:current','space',?1,'thread',?2,NULL,'address','alpha','agent_session',NULL,?1,'test','private','mention/v1',?2)",
            rusqlite::params![message, index],
        ).unwrap();
        connection.execute(
            "INSERT INTO message_recipients VALUES ('hc:current','space',?1,1,'address','alpha',NULL)",
            [&message],
        ).unwrap();
        connection.execute(
            "INSERT INTO message_recipients VALUES ('hc:current','space',?1,2,'other','beta',NULL)",
            [&message],
        ).unwrap();
        connection.execute(
            "INSERT INTO mailbox_deliveries VALUES ('hc:current','space',?1,'address',?2,?3,?4,?2)",
            rusqlite::params![delivery, index, message, state],
        ).unwrap();
        connection.execute(
            "INSERT INTO mailbox_deliveries VALUES ('hc:current','space',?1,'other',?2,?3,'queued',?2)",
            rusqlite::params![format!("other-{index}"), index, message],
        ).unwrap();
        connection.execute(
            "INSERT INTO delivery_attempts VALUES ('hc:current','space',?1,?2,'address','binding','session',NULL,?3)",
            rusqlite::params![format!("attempt-{index}-old"), delivery, index],
        ).unwrap();
        connection.execute(
            "INSERT INTO delivery_dispositions VALUES ('hc:current','space',?1,?2,?3,?4)",
            rusqlite::params![format!("disposition-{index}"), format!("attempt-{index}-old"), disposition, index],
        ).unwrap();
        if index == 3 {
            connection.execute(
                "INSERT INTO delivery_attempts VALUES ('hc:current','space','attempt-3-new','delivery-3','address','binding','session','attempt-3-old',4)",
                [],
            ).unwrap();
        }
    }
    connection.execute_batch(
        "INSERT INTO collaboration_messages (session_socket_id,worktree_id,message_id,thread_id,thread_order,in_reply_to,author_address_id,author_alias_snapshot,author_kind,author_session_snapshot,request_id,authored_source,body,grammar_version,created_at_ms)
           VALUES ('hc:current','space','queued-reply','thread',4,'request-3','other','beta','agent_session',NULL,'reply','test','private','mention/v1',5);
         INSERT INTO message_recipients VALUES ('hc:current','space','queued-reply',1,'address','alpha',NULL);
         INSERT INTO mailbox_deliveries VALUES ('hc:current','space','queued-return','address',4,'queued-reply','queued',5);",
    ).unwrap();
    let (output, _) = run_core(home.path(), json!({"core":{"id":"current"}}));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["rows"][0]["waiting_on"], json!([
        {"alias":"alpha","message_id":"request-3"},
        {"alias":"beta","message_id":"request-3"},
        {"alias":"alpha","message_id":"request-2"},
        {"alias":"beta","message_id":"request-2"},
        {"alias":"alpha","message_id":"request-1"},
        {"alias":"beta","message_id":"request-1"}
    ]));
    assert_eq!(body["rows"][0]["recent_replies"], json!([]));
}

#[test]
fn tui_roster_uses_hidden_command_rejection() {
    let home = PrivateHome::new();
    let output = run(home.path(), &home.path().join("absent.sock"), "tui");
    assert_eq!(output.status.code(), Some(64), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["error"]["message"], "roster requires --host hc");
}
