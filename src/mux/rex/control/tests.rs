use super::*;
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

struct Server {
    stream: UnixStream,
}

impl Server {
    fn upgrade(stream: UnixStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut server = Self { stream };
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            server.stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        server
            .stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n",
            )
            .unwrap();
        server
    }

    fn next_client_frame(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut head = [0; 2];
        self.stream.read_exact(&mut head).ok()?;
        let length = match head[1] & 0x7F {
            126 => {
                let mut length = [0; 2];
                self.stream.read_exact(&mut length).unwrap();
                u64::from(u16::from_be_bytes(length))
            }
            127 => {
                let mut length = [0; 8];
                self.stream.read_exact(&mut length).unwrap();
                u64::from_be_bytes(length)
            }
            length => u64::from(length),
        };
        let mut mask = [0; 4];
        self.stream.read_exact(&mut mask).unwrap();
        let mut payload = vec![0; length as usize];
        self.stream.read_exact(&mut payload).unwrap();
        for (byte, key) in payload.iter_mut().zip(mask.iter().cycle()) {
            *byte ^= key;
        }
        Some((head[0] & 0x0F, payload))
    }

    fn message(&mut self) -> Value {
        let (opcode, payload) = self.next_client_frame().expect("the client sent a message");
        assert_eq!(opcode, 0x1, "a text message");
        serde_json::from_slice(&payload).unwrap()
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) {
        assert!(
            self.try_send_frame(opcode, payload),
            "the client is reading"
        );
    }

    fn try_send_frame(&mut self, opcode: u8, payload: &[u8]) -> bool {
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            length @ 0..=125 => frame.push(length as u8),
            length @ 126..=0xFFFF => {
                frame.push(126);
                frame.extend_from_slice(&(length as u16).to_be_bytes());
            }
            length => {
                frame.push(127);
                frame.extend_from_slice(&(length as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(payload);
        self.stream.write_all(&frame).is_ok()
    }

    fn send(&mut self, message: Value) {
        self.send_frame(0x1, message.to_string().as_bytes());
    }

    fn welcome(&mut self) -> Value {
        let hello = self.message();
        self.send(json!({
            "type": "welcome",
            "protocol": "rex.control.v1",
            "api_revision": 12,
            "limits": {"max_message_bytes": 1_048_576},
            "client": {"id": "client:1", "reconnect_token": "secret"},
        }));
        hello
    }

    fn close_code(&mut self) -> Option<u16> {
        let (opcode, payload) = self.next_client_frame()?;
        assert_eq!(opcode, 0x8, "the client sent nothing but its close");
        Some(u16::from_be_bytes([payload[0], payload[1]]))
    }
}

fn scripted<T: Send + 'static>(
    script: impl FnOnce(&mut Server) -> T + Send + 'static,
) -> (UnixStream, JoinHandle<T>) {
    let (client, server) = UnixStream::pair().unwrap();
    let thread = thread::spawn(move || script(&mut Server::upgrade(server)));
    (client, thread)
}

fn budget(timeout_ms: u64, max_output_bytes: usize) -> Budget {
    Budget {
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes,
    }
}

fn open<'a>(
    stream: UnixStream,
    budget: &'a (dyn Fn() -> Result<Budget, String> + 'a),
    cancelled: &'a AtomicBool,
) -> Result<Connection<'a, UnixStream>, Fault> {
    Connection::open(stream, Limits { budget, cancelled })
}

const DEFAULT: &dyn Fn() -> Result<Budget, String> = &|| Ok(Budget::ADAPTER_DEFAULT);

#[test]
fn open_says_hello_reads_the_welcome_and_dropping_closes_with_1000() {
    let (client, server) = scripted(|server| {
        let hello = server.welcome();
        (hello, server.close_code())
    });
    let cancelled = AtomicBool::new(false);
    drop(open(client, DEFAULT, &cancelled).unwrap());
    let (hello, close) = server.join().unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["api_revision"], 12);
    assert_eq!(hello["api_min_revision"], 12);
    assert_eq!(hello["client"]["kind"], "other");
    assert_eq!(hello["client"]["name"], "namedrop");
    assert!(hello.to_string().len() < 512, "{hello}");
    assert_eq!(close, Some(1000));
}

#[test]
fn a_welcome_for_another_protocol_or_revision_is_refused() {
    for welcome in [
        json!({"type": "welcome", "protocol": "rex.control.v1", "api_revision": 11, "limits": {"max_message_bytes": 65536}}),
        json!({"type": "welcome", "protocol": "rex.control.v2", "api_revision": 12, "limits": {"max_message_bytes": 65536}}),
        json!({"type": "welcome", "protocol": "rex.control.v1", "limits": {"max_message_bytes": 65536}}),
        json!({"type": "event", "seq": 1, "payload": {}}),
    ] {
        let sent = welcome.clone();
        let (client, server) = scripted(move |server| {
            server.message();
            server.send(sent);
        });
        let cancelled = AtomicBool::new(false);
        let refused = open(client, DEFAULT, &cancelled).map(|_| ());
        server.join().unwrap();
        assert!(
            matches!(&refused, Err(Fault::Closed(error)) if error.contains("revision 12")),
            "{welcome}: {refused:?}"
        );
    }
}

#[test]
fn each_request_takes_only_its_own_response_and_the_events_wait() {
    let (client, server) = scripted(|server| {
        server.welcome();
        let first = server.message();
        server.send(json!({"type": "response", "id": "99", "seq": 0, "payload": {"stale": true}}));
        server.send(json!({"type": "event", "seq": 1, "payload": {"type": "session_created"}}));
        server.send(json!({"type": "notice", "text": "a type this client does not know"}));
        server.send(
            json!({"type": "response", "id": first["id"], "seq": 1, "payload": {"pid": 4680}}),
        );
        let second = server.message();
        server.send(json!({"type": "event", "seq": 2, "payload": {"type": "bell"}}));
        server.send(json!({
            "type": "response", "id": second["id"], "seq": 2,
            "error": {"code": "not_found", "message": "block not found", "status": 404},
        }));
        (first, second, server.close_code())
    });
    let cancelled = AtomicBool::new(false);
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Ok(Reply {
            seq: 1,
            payload: json!({"pid": 4680})
        })
    );
    assert_eq!(
        connection.request("process", Some(json!({"block_id": "block:b"}))),
        Err(Fault::Rex {
            code: "not_found".into(),
            message: "block not found".into(),
            status: 404
        })
    );
    let seqs: Vec<_> = [(); 2]
        .iter()
        .map(|()| connection.next_event(Duration::ZERO).unwrap().unwrap())
        .map(|event| (event.seq, event.payload["type"].clone()))
        .collect();
    assert_eq!(seqs, [(1, json!("session_created")), (2, json!("bell"))]);
    assert_eq!(connection.next_event(Duration::from_millis(30)), Ok(None));
    drop(connection);
    let (first, second, close) = server.join().unwrap();
    assert_eq!(close, Some(1000));
    assert_eq!(
        first,
        json!({"type": "request", "id": "1", "method": "server.status"})
    );
    assert_eq!(
        second,
        json!({"type": "request", "id": "2", "method": "process", "payload": {"block_id": "block:b"}})
    );
}

#[test]
fn an_event_that_arrives_while_nothing_waits_is_read_by_next_event() {
    let (client, server) = scripted(|server| {
        server.welcome();
        thread::sleep(Duration::from_millis(50));
        server.send(json!({"type": "event", "seq": 7, "payload": {"type": "block_closed"}}));
        server.close_code()
    });
    let cancelled = AtomicBool::new(false);
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    let event = connection.next_event(Duration::from_secs(5)).unwrap();
    assert_eq!(
        event,
        Some(Event {
            seq: 7,
            payload: json!({"type": "block_closed"})
        })
    );
    drop(connection);
    assert_eq!(server.join().unwrap(), Some(1000));
}

#[test]
fn a_request_with_no_answer_times_out_at_its_budget() {
    let (client, server) = scripted(|server| {
        server.welcome();
        server.message();
        server.close_code()
    });
    let cancelled = AtomicBool::new(false);
    let short = || Ok(budget(150, usize::MAX));
    let mut connection = open(client, &short, &cancelled).unwrap();
    let started = Instant::now();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Timeout(Duration::from_millis(150)))
    );
    let waited = started.elapsed();
    assert!(
        (Duration::from_millis(150)..Duration::from_secs(2)).contains(&waited),
        "{waited:?}"
    );
    drop(connection);
    assert_eq!(server.join().unwrap(), Some(1000));
}

#[test]
fn the_timeout_cap_defaults_to_5_s_and_stops_at_60_s() {
    for (value, cap) in [
        (None, 5000),
        (Some("250"), 250),
        (Some("0"), 5000),
        (Some("soon"), 5000),
        (Some("999999"), 60_000),
    ] {
        assert_eq!(timeout_cap(value), Duration::from_millis(cap), "{value:?}");
    }
}

#[test]
fn a_set_flag_stops_a_waiting_request_within_50_ms_and_refuses_the_next_unsent() {
    let (client, server) = scripted(|server| {
        server.welcome();
        let request = server.message();
        (request, server.close_code())
    });
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    let setter = {
        let cancelled = Arc::clone(&cancelled);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancelled.store(true, Ordering::SeqCst);
            Instant::now()
        })
    };
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Cancelled)
    );
    let stopped = Instant::now();
    let set = setter.join().unwrap();
    let late = stopped.saturating_duration_since(set);
    assert!(late < Duration::from_millis(50), "{late:?}");
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Cancelled)
    );
    drop(connection);
    let (request, close) = server.join().unwrap();
    assert_eq!(request["id"], "1");
    assert_eq!(close, Some(1000), "the second request was never sent");
}

#[test]
fn an_allowance_that_cannot_give_a_budget_stops_the_request_unsent() {
    let (client, server) = scripted(|server| {
        server.welcome();
        server.close_code()
    });
    let cancelled = AtomicBool::new(false);
    let asked = AtomicBool::new(false);
    let budget = || {
        if asked.swap(true, Ordering::SeqCst) {
            Err("the budget ran out".to_owned())
        } else {
            Ok(Budget::ADAPTER_DEFAULT)
        }
    };
    let mut connection = open(client, &budget, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Allowance("the budget ran out".into()))
    );
    drop(connection);
    assert_eq!(server.join().unwrap(), Some(1000));
}

fn padded_event(seq: u64) -> Value {
    json!({"type": "event", "seq": seq, "payload": {"type": "title_changed", "title": "t".repeat(940)}})
}

#[test]
fn a_request_receives_no_more_than_its_budget_and_the_call_no_more_than_4_mib() {
    let (client, server) = scripted(|server| {
        server.welcome();
        server.message();
        for seq in 1..=3 {
            server.send(padded_event(seq));
        }
        server.close_code()
    });
    let cancelled = AtomicBool::new(false);
    let capped = || Ok(budget(5000, 2500));
    let mut connection = open(client, &capped, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::TooLarge)
    );
    drop(connection);
    assert_eq!(server.join().unwrap(), Some(1000));

    let events = MAX_CALL_BYTES / 1000 + 1;
    let (client, server) = scripted(move |server| {
        server.welcome();
        server.message();
        let response = json!({"type": "response", "id": "1", "seq": events, "payload": {}});
        let _ = (1..=events as u64)
            .map(padded_event)
            .chain([response])
            .all(|message| server.try_send_frame(0x1, message.to_string().as_bytes()));
    });
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::TooLarge)
    );
    let buffered =
        std::iter::from_fn(|| connection.next_event(Duration::ZERO).ok().flatten()).count();
    assert!(
        (MAX_CALL_BYTES / 1100..MAX_CALL_BYTES / 1000).contains(&buffered),
        "{buffered} events of about 1 KB fit in 4 MiB"
    );
    drop(connection);
    server.join().unwrap();
}

#[test]
fn a_request_larger_than_the_server_accepts_is_refused_unsent() {
    let (client, server) = scripted(|server| {
        server.message();
        server.send(json!({
            "type": "welcome", "protocol": "rex.control.v1", "api_revision": 12,
            "limits": {"max_message_bytes": 65536},
        }));
        server.close_code()
    });
    let cancelled = AtomicBool::new(false);
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    let text = "p".repeat(70 * 1024);
    assert_eq!(
        connection.request("block.event", Some(json!({"type": "paste", "text": text}))),
        Err(Fault::TooLarge)
    );
    drop(connection);
    assert_eq!(server.join().unwrap(), Some(1000));
}

#[test]
fn a_server_that_closes_mid_request_ends_the_connection() {
    let (client, server) = scripted(|server| {
        server.welcome();
        server.message();
    });
    let cancelled = AtomicBool::new(false);
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Closed("the connection ended".into()))
    );
    server.join().unwrap();

    let (client, server) = scripted(|server| {
        server.welcome();
        server.message();
        let mut close = 1008u16.to_be_bytes().to_vec();
        close.extend_from_slice(b"event backlog exceeded");
        server.send_frame(0x8, &close);
        server.close_code()
    });
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::Closed(
            "the server closed the connection (1008 event backlog exceeded)".into()
        ))
    );
    drop(connection);
    assert_eq!(
        server.join().unwrap(),
        Some(1008),
        "the close was echoed once"
    );
}

#[test]
fn the_4_mib_count_spans_every_request_on_the_connection() {
    let (client, server) = scripted(|server| {
        server.welcome();
        for (id, first) in [("1", 1u64), ("2", 2601)] {
            server.message();
            let events = (first..first + 2600).map(padded_event);
            let response =
                json!({"type": "response", "id": id, "seq": first + 2599, "payload": {}});
            let _ = events
                .chain([response])
                .all(|message| server.try_send_frame(0x1, message.to_string().as_bytes()));
        }
    });
    let cancelled = AtomicBool::new(false);
    let mut connection = open(client, DEFAULT, &cancelled).unwrap();
    assert!(connection.request("server.status", None).is_ok());
    assert_eq!(
        connection.request("server.status", None),
        Err(Fault::TooLarge)
    );
    drop(connection);
    server.join().unwrap();
}
