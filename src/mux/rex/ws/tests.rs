use super::*;
use std::{cell::Cell, io::Cursor};

const ANSWER: &str = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                      Connection: Upgrade\r\nsec-websocket-protocol: rex.control.v1\r\n\r\n";

struct Pipe {
    from_server: Cursor<Vec<u8>>,
    to_server: Vec<u8>,
    byte_at_a_time_from: Option<u64>,
    timed_out: bool,
    writes_time_out: Cell<bool>,
}

impl Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(from) = self.byte_at_a_time_from else {
            return self.from_server.read(buffer);
        };
        let position = self.from_server.position();
        if position < from {
            let end = buffer.len().min((from - position) as usize);
            return self.from_server.read(&mut buffer[..end]);
        }
        self.timed_out = !self.timed_out;
        if self.timed_out {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let end = buffer.len().min(1);
        self.from_server.read(&mut buffer[..end])
    }
}

impl Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.writes_time_out.get() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.to_server.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![u8::from(fin) << 7 | opcode];
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
    frame
}

fn server(frames: &[Vec<u8>]) -> Pipe {
    let mut bytes = ANSWER.as_bytes().to_vec();
    bytes.extend(frames.concat());
    Pipe {
        from_server: Cursor::new(bytes),
        to_server: Vec::new(),
        byte_at_a_time_from: None,
        timed_out: false,
        writes_time_out: Cell::new(false),
    }
}

fn connect(pipe: Pipe) -> WebSocket<Pipe> {
    let mut socket = WebSocket::connect(pipe, "/api/control", "rex.control.v1", 1 << 20).unwrap();
    socket.upgrade().unwrap();
    socket
}

fn client_frames(socket: &WebSocket<Pipe>) -> Vec<(u8, Vec<u8>, u8, [u8; 4])> {
    let bytes = &socket.stream().to_server;
    let start = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let mut rest = &bytes[start..];
    let mut frames = Vec::new();
    while !rest.is_empty() {
        assert_eq!(rest[0] & 0x80, 0x80, "every client frame is final");
        assert_eq!(rest[1] & 0x80, 0x80, "every client frame is masked");
        let form = rest[1] & 0x7F;
        let (header, length) = match form {
            126 => (4, usize::from(u16::from_be_bytes([rest[2], rest[3]]))),
            127 => (
                10,
                u64::from_be_bytes(rest[2..10].try_into().unwrap()) as usize,
            ),
            length => (2, usize::from(length)),
        };
        let mask: [u8; 4] = rest[header..header + 4].try_into().unwrap();
        let payload = rest[header + 4..header + 4 + length]
            .iter()
            .zip(mask.iter().cycle())
            .map(|(byte, key)| byte ^ key)
            .collect();
        frames.push((rest[0] & 0x0F, payload, form, mask));
        rest = &rest[header + 4 + length..];
    }
    frames
}

#[test]
fn the_upgrade_asks_for_the_subprotocol_and_keeps_what_follows_the_answer() {
    let mut socket = connect(server(&[frame(true, TEXT, b"hi")]));
    let request = String::from_utf8(socket.stream().to_server.clone()).unwrap();
    let mut lines = request.lines();
    assert_eq!(lines.next(), Some("GET /api/control HTTP/1.1"));
    let headers: Vec<_> = lines.take_while(|line| !line.is_empty()).collect();
    for header in [
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
        "Sec-WebSocket-Protocol: rex.control.v1",
    ] {
        assert!(headers.contains(&header), "{header} in {headers:?}");
    }
    let key = headers
        .iter()
        .find_map(|header| header.strip_prefix("Sec-WebSocket-Key: "))
        .unwrap();
    assert_eq!(key.len(), 24, "16 bytes in base64: {key}");
    assert!(key.ends_with("=="), "{key}");
    assert!(!headers.iter().any(|header| header.contains("Extensions")));
    assert_eq!(socket.recv().unwrap(), Message::Text("hi".into()));
}

#[test]
fn an_answer_that_is_not_an_upgrade_to_the_subprotocol_is_refused() {
    for (answer, said) in [
        (
            "HTTP/1.1 403 Forbidden\r\n\r\n",
            "the server answered the upgrade with \"HTTP/1.1 403 Forbidden\"",
        ),
        (
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n",
            "the server did not accept the rex.control.v1 subprotocol",
        ),
        (
            "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Protocol: other\r\n\r\n",
            "the server did not accept the rex.control.v1 subprotocol",
        ),
    ] {
        let pipe = Pipe {
            from_server: Cursor::new(answer.as_bytes().to_vec()),
            ..server(&[])
        };
        let mut socket =
            WebSocket::connect(pipe, "/api/control", "rex.control.v1", 1 << 20).unwrap();
        match socket.upgrade() {
            Err(WsError::Protocol(error)) => assert_eq!(error, said),
            Err(error) => panic!("{answer:?}: {error:?}"),
            Ok(_) => panic!("{answer:?} was taken for an upgrade"),
        }
    }
    let cut = Pipe {
        from_server: Cursor::new(b"HTTP/1.1 101 Switching".to_vec()),
        ..server(&[])
    };
    let mut socket = WebSocket::connect(cut, "/api/control", "rex.control.v1", 1 << 20).unwrap();
    assert!(matches!(
        socket.upgrade(),
        Err(WsError::Closed { code: None, .. })
    ));
}

#[test]
fn an_upgrade_answer_read_across_timeouts_is_taken_whole_and_nothing_is_sent_before_it() {
    let mut pipe = server(&[frame(true, TEXT, b"hi")]);
    pipe.byte_at_a_time_from = Some(0);
    let mut socket = WebSocket::connect(pipe, "/api/control", "rex.control.v1", 1 << 20).unwrap();
    socket.close(1000).unwrap();
    let mut timeouts = 0;
    while let Err(error) = socket.upgrade() {
        assert!(
            matches!(&error, WsError::Io(error) if error.kind() == io::ErrorKind::WouldBlock),
            "{error:?}"
        );
        timeouts += 1;
    }
    assert!(timeouts >= ANSWER.len(), "{timeouts}");
    let message = loop {
        match socket.recv() {
            Ok(message) => break message,
            Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{error:?}"),
        }
    };
    assert_eq!(message, Message::Text("hi".into()));
    assert!(
        client_frames(&socket).is_empty(),
        "no close before the upgrade was answered"
    );
}

#[test]
fn each_frame_is_masked_with_its_own_key_in_the_shortest_length_form() {
    let mut socket = connect(server(&[]));
    let long = "x".repeat(70 * 1024);
    let medium = "y".repeat(300);
    for text in ["hello", "hello", &medium, &long] {
        socket.send_text(text).unwrap();
    }
    let frames = client_frames(&socket);
    let shapes: Vec<_> = frames
        .iter()
        .map(|(opcode, payload, form, _)| (*opcode, payload.len(), *form))
        .collect();
    assert_eq!(
        shapes,
        [
            (TEXT, 5, 5),
            (TEXT, 5, 5),
            (TEXT, 300, 126),
            (TEXT, 70 * 1024, 127)
        ]
    );
    assert_eq!(frames[0].1, b"hello");
    assert_eq!(frames[3].1, long.as_bytes());
    assert_ne!(frames[0].3, frames[1].3, "a new key for each frame");
}

#[test]
fn a_70_kib_message_with_a_64_bit_length_arrives_whole() {
    let text = "z".repeat(70 * 1024);
    let mut socket = connect(server(&[frame(true, TEXT, text.as_bytes())]));
    assert_eq!(socket.recv().unwrap(), Message::Text(text));
}

#[test]
fn a_fragmented_message_arrives_whole_and_a_ping_between_its_fragments_is_answered() {
    let mut socket = connect(server(&[
        frame(false, TEXT, b"hel"),
        frame(true, PING, b"are you there"),
        frame(false, CONTINUATION, b"l"),
        frame(true, PONG, b"stray"),
        frame(true, CONTINUATION, b"o"),
        frame(true, BINARY, &[0, 1]),
    ]));
    assert_eq!(socket.recv().unwrap(), Message::Text("hello".into()));
    assert_eq!(socket.recv().unwrap(), Message::Binary(vec![0, 1]));
    let answered: Vec<_> = client_frames(&socket)
        .into_iter()
        .map(|(opcode, payload, ..)| (opcode, payload))
        .collect();
    assert_eq!(answered, [(PONG, b"are you there".to_vec())]);
}

#[test]
fn a_pong_that_cannot_be_sent_ends_the_connection_instead_of_reading_as_a_timeout() {
    let mut socket = connect(server(&[frame(true, PING, b"there?")]));
    socket.stream().writes_time_out.set(true);
    assert!(matches!(
        socket.recv(),
        Err(WsError::Closed { code: None, reason }) if reason == "the pong could not be sent"
    ));
}

#[test]
fn a_close_frame_is_answered_once_and_reported_with_its_code() {
    let mut payload = 1001u16.to_be_bytes().to_vec();
    payload.extend_from_slice(b"keepalive timeout");
    let mut socket = connect(server(&[frame(true, CLOSE, &payload)]));
    match socket.recv() {
        Err(WsError::Closed { code, reason }) => {
            assert_eq!((code, reason.as_str()), (Some(1001), "keepalive timeout"));
        }
        other => panic!("{other:?}"),
    }
    socket.close(1000).unwrap();
    let closes: Vec<_> = client_frames(&socket)
        .into_iter()
        .map(|(opcode, payload, ..)| (opcode, payload))
        .collect();
    assert_eq!(closes, [(CLOSE, 1001u16.to_be_bytes().to_vec())]);

    let mut socket = connect(server(&[]));
    socket.close(1000).unwrap();
    socket.close(1000).unwrap();
    let closes: Vec<_> = client_frames(&socket)
        .into_iter()
        .map(|(opcode, payload, ..)| (opcode, payload))
        .collect();
    assert_eq!(closes, [(CLOSE, 1000u16.to_be_bytes().to_vec())]);
    assert!(matches!(
        socket.recv(),
        Err(WsError::Closed { code: None, .. })
    ));
}

#[test]
fn a_message_over_the_limit_is_refused_by_its_header_before_its_payload_arrives() {
    let mut header = vec![0x80 | TEXT, 127];
    header.extend_from_slice(&(2048u64).to_be_bytes());
    let mut socket = WebSocket::connect(server(&[header]), "/", "rex.control.v1", 1024).unwrap();
    assert!(matches!(socket.recv(), Err(WsError::TooLarge)));

    let mut socket = WebSocket::connect(
        server(&[
            frame(false, TEXT, &[b'a'; 600]),
            frame(true, CONTINUATION, &[b'b'; 600]),
        ]),
        "/",
        "rex.control.v1",
        1024,
    )
    .unwrap();
    assert!(matches!(socket.recv(), Err(WsError::TooLarge)));
}

#[test]
fn a_frame_over_the_allowance_is_refused_by_its_header_before_its_payload_arrives() {
    let mut header = vec![0x80 | TEXT, 126];
    header.extend_from_slice(&600u16.to_be_bytes());
    let mut socket = connect(server(&[header]));
    socket.set_allowance(100);
    assert!(matches!(socket.recv(), Err(WsError::TooLarge)));
}

#[test]
fn the_allowance_counts_every_frame_byte_and_refuses_the_frame_that_crosses_it() {
    let mut socket = connect(server(&[
        frame(true, TEXT, b"hi"),
        frame(true, PING, b""),
        frame(true, TEXT, b"again"),
    ]));
    socket.set_allowance(10);
    assert_eq!(socket.recv().unwrap(), Message::Text("hi".into()));
    assert_eq!(socket.allowance(), 6);
    assert!(matches!(socket.recv(), Err(WsError::TooLarge)));
    assert_eq!(
        socket.allowance(),
        4,
        "the ping was consumed, the text was not"
    );
    socket.set_allowance(6);
    assert!(
        matches!(socket.recv(), Err(WsError::TooLarge)),
        "a 5-byte payload with its 2-byte header is 7 bytes"
    );
    socket.set_allowance(7);
    assert_eq!(socket.recv().unwrap(), Message::Text("again".into()));
}

#[test]
fn a_read_that_stops_mid_frame_resumes_where_it_stopped() {
    let text = "r".repeat(300);
    let mut pipe = server(&[
        frame(false, TEXT, b"ab"),
        frame(true, CONTINUATION, text.as_bytes()),
    ]);
    pipe.byte_at_a_time_from = Some(ANSWER.len() as u64);
    let mut socket = WebSocket::connect(pipe, "/", "rex.control.v1", 1 << 20).unwrap();
    let mut timeouts = 0;
    let message = loop {
        match socket.recv() {
            Ok(message) => break message,
            Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => timeouts += 1,
            Err(error) => panic!("{error:?}"),
        }
    };
    assert_eq!(message, Message::Text(format!("ab{text}")));
    assert!(timeouts > 300, "{timeouts}");
}

#[test]
fn a_frame_that_breaks_the_protocol_is_refused() {
    let mut masked = frame(true, TEXT, b"hi");
    masked[1] |= 0x80;
    let mut reserved = frame(true, TEXT, b"hi");
    reserved[0] |= 0x40;
    for frames in [
        vec![masked],
        vec![reserved],
        vec![frame(true, CONTINUATION, b"x")],
        vec![frame(false, TEXT, b"a"), frame(true, TEXT, b"b")],
        vec![frame(false, PING, b"")],
        vec![frame(true, PING, &[0; 126])],
        vec![frame(true, 0x3, b"")],
        vec![frame(true, TEXT, &[0xFF, 0xFE])],
    ] {
        let mut socket = connect(server(&frames));
        assert!(
            matches!(socket.recv(), Err(WsError::Protocol(_))),
            "{frames:?}"
        );
    }
}

#[test]
fn base64_encodes_with_padding() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(&[0xFF; 16]), "/////////////////////w==");
}
