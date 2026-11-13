use std::io::{self, Read, Write};

const CONTINUATION: u8 = 0x0;
const TEXT: u8 = 0x1;
const BINARY: u8 = 0x2;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

const MAX_HEAD_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub(super) enum WsError {
    Io(io::Error),
    Closed { code: Option<u16>, reason: String },
    TooLarge,
    Protocol(String),
}

impl From<io::Error> for WsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Message {
    Text(String),
    Binary(Vec<u8>),
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

pub(super) struct WebSocket<S> {
    stream: S,
    buffer: Vec<u8>,
    fragmented: Option<(u8, Vec<u8>)>,
    max_message: usize,
    /// Frame bytes left to consume, headers included.
    allowance: usize,
    close_sent: bool,
    /// The subprotocol asked for, until the server's answer to the upgrade is read.
    upgrade: Option<String>,
}

impl<S: Read + Write> WebSocket<S> {
    /// Sends the upgrade request. `upgrade`, or the first `recv` or
    /// `send_text`, reads the answer. Skips the `Sec-WebSocket-Accept` check,
    /// which needs SHA-1, which no dependency provides. The Rex adapter dials
    /// only a Unix socket on this machine.
    pub(super) fn connect(
        mut stream: S,
        path: &str,
        protocol: &str,
        max_message: usize,
    ) -> Result<Self, WsError> {
        let key = base64(&random::<16>()?);
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
            fragmented: None,
            max_message,
            allowance: usize::MAX,
            close_sent: false,
            upgrade: Some(protocol.to_owned()),
        })
    }

    /// Reads the server's answer to the upgrade, once. A read that times out
    /// keeps what it read, so the next call resumes it.
    pub(super) fn upgrade(&mut self) -> Result<(), WsError> {
        let Some(protocol) = self.upgrade.clone() else {
            return Ok(());
        };
        let end = loop {
            if let Some(end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break end;
            }
            if self.buffer.len() > MAX_HEAD_BYTES {
                return Err(WsError::Protocol(
                    "the upgrade answer's headers are too long".into(),
                ));
            }
            self.fill()?;
        };
        let head = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
        self.buffer.drain(..end + 4);
        let status = head.split("\r\n").next().unwrap_or_default();
        if status.split_whitespace().nth(1) != Some("101") {
            return Err(WsError::Protocol(format!(
                "the server answered the upgrade with {status:?}"
            )));
        }
        let header = |name: &str| {
            head.split("\r\n").skip(1).find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_owned())
            })
        };
        if header("Sec-WebSocket-Protocol").as_deref() != Some(protocol.as_str()) {
            return Err(WsError::Protocol(format!(
                "the server did not accept the {protocol} subprotocol"
            )));
        }
        self.upgrade = None;
        Ok(())
    }

    pub(super) fn stream(&self) -> &S {
        &self.stream
    }

    pub(super) fn allowance(&self) -> usize {
        self.allowance
    }

    pub(super) fn set_allowance(&mut self, bytes: usize) {
        self.allowance = bytes;
    }

    pub(super) fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.upgrade()?;
        self.write_frame(TEXT, text.as_bytes())
    }

    /// Sends nothing before the upgrade is answered, because no WebSocket
    /// exists yet to close.
    pub(super) fn close(&mut self, code: u16) -> Result<(), WsError> {
        if self.close_sent || self.upgrade.is_some() {
            return Ok(());
        }
        self.close_sent = true;
        self.write_frame(CLOSE, &code.to_be_bytes())
    }

    pub(super) fn recv(&mut self) -> Result<Message, WsError> {
        self.upgrade()?;
        loop {
            let Some(frame) = self.take_frame()? else {
                self.fill()?;
                continue;
            };
            match frame.opcode {
                // A failed write may have sent part of the pong, so it ends
                // the connection rather than reading as a read that timed out.
                PING => {
                    self.write_frame(PONG, &frame.payload)
                        .map_err(|_| WsError::Closed {
                            code: None,
                            reason: "the pong could not be sent".into(),
                        })?;
                }
                PONG => {}
                CLOSE => {
                    let code = frame
                        .payload
                        .get(..2)
                        .map(|code| u16::from_be_bytes([code[0], code[1]]));
                    let reason =
                        String::from_utf8_lossy(frame.payload.get(2..).unwrap_or(&[])).into_owned();
                    if !self.close_sent {
                        self.close_sent = true;
                        let _ = self.write_frame(CLOSE, frame.payload.get(..2).unwrap_or(&[]));
                    }
                    return Err(WsError::Closed { code, reason });
                }
                TEXT | BINARY if self.fragmented.is_some() => {
                    return Err(WsError::Protocol(
                        "a message began inside a fragmented one".into(),
                    ));
                }
                TEXT | BINARY if frame.fin => return message(frame.opcode, frame.payload),
                TEXT | BINARY => self.fragmented = Some((frame.opcode, frame.payload)),
                CONTINUATION => {
                    let Some((opcode, mut payload)) = self.fragmented.take() else {
                        return Err(WsError::Protocol(
                            "a continuation frame with no message".into(),
                        ));
                    };
                    payload.extend(frame.payload);
                    if frame.fin {
                        return message(opcode, payload);
                    }
                    self.fragmented = Some((opcode, payload));
                }
                opcode => {
                    return Err(WsError::Protocol(format!("a frame with opcode {opcode}")));
                }
            }
        }
    }

    fn take_frame(&mut self) -> Result<Option<Frame>, WsError> {
        let [first, second, ..] = self.buffer[..] else {
            return Ok(None);
        };
        if first & 0x70 != 0 {
            return Err(WsError::Protocol("a frame with reserved bits set".into()));
        }
        if second & 0x80 != 0 {
            return Err(WsError::Protocol("the server masked a frame".into()));
        }
        let (fin, opcode) = (first & 0x80 != 0, first & 0x0F);
        let (header, length) = match second & 0x7F {
            126 if self.buffer.len() >= 4 => (
                4,
                u64::from(u16::from_be_bytes([self.buffer[2], self.buffer[3]])),
            ),
            127 if self.buffer.len() >= 10 => {
                let mut length = [0; 8];
                length.copy_from_slice(&self.buffer[2..10]);
                (10, u64::from_be_bytes(length))
            }
            126 | 127 => return Ok(None),
            length => (2, u64::from(length)),
        };
        if opcode >= CLOSE && (!fin || length > 125) {
            return Err(WsError::Protocol(
                "a control frame that is fragmented or too long".into(),
            ));
        }
        let length = usize::try_from(length).map_err(|_| WsError::TooLarge)?;
        let message = self
            .fragmented
            .as_ref()
            .map_or(0, |(_, payload)| payload.len());
        if opcode < CLOSE && message.saturating_add(length) > self.max_message {
            return Err(WsError::TooLarge);
        }
        let size = header + length;
        if size > self.allowance {
            return Err(WsError::TooLarge);
        }
        if self.buffer.len() < size {
            return Ok(None);
        }
        let payload = self.buffer[header..size].to_vec();
        self.buffer.drain(..size);
        self.allowance -= size;
        Ok(Some(Frame {
            fin,
            opcode,
            payload,
        }))
    }

    fn fill(&mut self) -> Result<(), WsError> {
        let mut chunk = [0; 64 * 1024];
        let read = self.stream.read(&mut chunk)?;
        if read == 0 {
            return Err(WsError::Closed {
                code: None,
                reason: "the connection ended".into(),
            });
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(())
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        match payload.len() {
            length @ 0..=125 => frame.push(0x80 | length as u8),
            length @ 126..=0xFFFF => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(length as u16).to_be_bytes());
            }
            length => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(length as u64).to_be_bytes());
            }
        }
        let mask = random::<4>()?;
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .zip(mask.iter().cycle())
                .map(|(byte, key)| byte ^ key),
        );
        self.stream.write_all(&frame)?;
        self.stream.flush()?;
        Ok(())
    }
}

fn message(opcode: u8, payload: Vec<u8>) -> Result<Message, WsError> {
    if opcode == BINARY {
        return Ok(Message::Binary(payload));
    }
    String::from_utf8(payload)
        .map(Message::Text)
        .map_err(|_| WsError::Protocol("a text message that is not UTF-8".into()))
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    const { assert!(N <= 256) };
    let mut bytes = [0; N];
    // SAFETY: `bytes` is a writable buffer of `N` bytes, and `N` is at most
    // 256, the most getentropy fills in one call.
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), N) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |triple, (index, &byte)| {
                triple | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(triple >> (18 - 6 * index) & 0x3F) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests;
