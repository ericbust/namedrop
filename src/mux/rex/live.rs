use super::ws::{Message, WebSocket};
use std::{os::unix::net::UnixStream, path::PathBuf};

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
