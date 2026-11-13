use super::{
    control::{Connection, Fault, Limits},
    ws::{Message, WebSocket},
};
use crate::mux::Budget;
use serde_json::json;
use std::{os::unix::net::UnixStream, path::PathBuf, sync::atomic::AtomicBool};

fn socket() -> PathBuf {
    let url = std::env::var("NAMEDROP_REX_LIVE")
        .expect("set NAMEDROP_REX_LIVE to a scratch Rex server's unix:// URL");
    PathBuf::from(
        url.strip_prefix("unix://")
            .expect("NAMEDROP_REX_LIVE is a unix:// URL"),
    )
}

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn ws_reads_the_welcome_of_a_real_server() {
    let stream = UnixStream::connect(socket()).unwrap();
    let mut socket = WebSocket::connect(stream, "/api/control", "rex.control.v1", 1 << 20).unwrap();
    socket
        .send_text(r#"{"type":"hello","api_revision":12,"client":{"kind":"other","name":"namedrop-live"}}"#)
        .unwrap();
    let Message::Text(welcome) = socket.recv().unwrap() else {
        panic!("the welcome is a text message");
    };
    let welcome: serde_json::Value = serde_json::from_str(&welcome).unwrap();
    assert_eq!(welcome["type"], "welcome", "{welcome}");
    assert_eq!(welcome["protocol"], "rex.control.v1", "{welcome}");
    assert_eq!(welcome["api_revision"], 12, "{welcome}");
    socket.close(1000).unwrap();
}

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn control_reads_the_server_status_and_a_typed_error() {
    let cancelled = AtomicBool::new(false);
    let budget = || Ok(Budget::ADAPTER_DEFAULT);
    let mut connection = Connection::open(
        UnixStream::connect(socket()).unwrap(),
        Limits {
            budget: &budget,
            cancelled: &cancelled,
        },
    )
    .unwrap();
    let status = connection.request("server.status", None).unwrap();
    assert_eq!(status.seq, 0, "no events before the first request");
    assert!(
        status.payload["pid"].as_u64().is_some(),
        "{}",
        status.payload
    );
    let missing = json!({"session_id": "session:00000000-0000-0000-0000-000000000000"});
    match connection.request("session.attach", Some(missing)) {
        Err(Fault::Rex { code, status, .. }) => {
            assert_eq!((code.as_str(), status), ("session_not_found", 404));
        }
        other => panic!("{other:?}"),
    }
}
