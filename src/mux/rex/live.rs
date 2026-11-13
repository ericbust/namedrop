use super::{
    control::{Connection, Fault, Limits},
    local::{peer_pid, prove_local, tests::forward},
    ws::{Message, WebSocket},
};
use crate::mux::{Budget, MuxKind};
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
    socket.upgrade().unwrap();
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

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn the_scratch_server_is_local_and_a_socket_forwarded_to_it_is_not() {
    let cancelled = AtomicBool::new(false);
    let budget = || Ok(Budget::ADAPTER_DEFAULT);
    let prove = |dial: &std::path::Path| {
        let stream = UnixStream::connect(dial).unwrap();
        let peer = peer_pid(&stream).unwrap();
        let limits = Limits {
            budget: &budget,
            cancelled: &cancelled,
        };
        let mut control = Connection::open(stream, limits).unwrap();
        (peer, prove_local(dial, &mut control, peer))
    };
    let (peer, server) = prove(&socket());
    let server = server.unwrap();
    assert_eq!(server.pid, peer);
    assert_eq!(server.endpoint.kind, MuxKind::Rex);
    assert_eq!(server.endpoint.unix_socket(), Some(socket().as_path()));
    assert!(server.host_id.is_some());

    let forwarded = std::env::temp_dir().join(format!("nd-live-fwd-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&forwarded);
    let forwarder = forward(&forwarded, socket());
    let (peer, refused) = prove(&forwarded);
    assert_eq!(peer, std::process::id());
    match refused {
        Err(Fault::NotLocal(why)) => assert!(
            why.contains(&format!("the server is process {}", server.pid)),
            "{why}"
        ),
        other => panic!(
            "a forwarded socket was proved local: {:?}",
            other.map(|server| server.endpoint)
        ),
    }
    forwarder.join().unwrap();
    std::fs::remove_file(&forwarded).unwrap();
}
