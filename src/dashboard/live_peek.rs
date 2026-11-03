//! The private, receive-only terminal projection boundary for one HTTP viewer.
use serde::Serialize;

const ENCODED_LIMIT: usize = 2 * 1024 * 1024;
const PAINT_LIMIT: usize = 1024 * 1024;
const DATA_LIMIT: usize = 64 * 1024;
const PENDING_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Serialize)]
struct Record<'a> {
    v: u8,
    seq: u64,
    #[serde(flatten)]
    event: Event<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    Reset {
        continuity: &'a str,
        cols: u16,
        rows: u16,
        bytes: &'a str,
    },
    Data {
        bytes: &'a str,
    },
    Closed {
        reason: Closed,
    },
    Unavailable {
        reason: Unavailable,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Closed {
    Ended,
    Replaced,
    Missing,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Unavailable {
    Unsupported,
    Busy,
    SourceLost,
    InvalidSource,
    SlowViewer,
    ServerStopping,
}

impl Record<'_> {
    fn line(seq: u64, event: Event<'_>) -> Result<Vec<u8>, ()> {
        let mut line = serde_json::to_vec(&Record { v: 1, seq, event }).map_err(|_| ())?;
        if line.len() > ENCODED_LIMIT {
            return Err(());
        }
        line.push(b'\n');
        Ok(line)
    }
}

fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(DIGITS[usize::from(a >> 2)] as char);
        output.push(DIGITS[usize::from((a & 3) << 4 | b >> 4)] as char);
        output.push(if chunk.len() > 1 {
            DIGITS[usize::from((b & 15) << 2 | c >> 6)] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            DIGITS[usize::from(c & 63)] as char
        } else {
            '='
        });
    }
    output
}

fn decode(text: &str) -> Result<Vec<u8>, ()> {
    fn digit(value: u8) -> Result<u8, ()> {
        match value {
            b'A'..=b'Z' => Ok(value - b'A'),
            b'a'..=b'z' => Ok(value - b'a' + 26),
            b'0'..=b'9' => Ok(value - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(()),
        }
    }
    if !text.len().is_multiple_of(4) || text.len() > PAINT_LIMIT.div_ceil(3) * 4 {
        return Err(());
    }
    let mut output = Vec::with_capacity((text.len() / 4 * 3).min(PAINT_LIMIT));
    let count = text.len() / 4;
    for (i, part) in text.as_bytes().chunks_exact(4).enumerate() {
        let a = digit(part[0])?;
        let b = digit(part[1])?;
        output.push(a << 2 | b >> 4);
        if part[2] == b'=' {
            if part[3] != b'=' || i + 1 != count || b & 15 != 0 {
                return Err(());
            }
        } else {
            let c = digit(part[2])?;
            output.push(b << 4 | c >> 2);
            if part[3] == b'=' {
                if i + 1 != count || c & 3 != 0 {
                    return Err(());
                }
            } else {
                output.push(c << 6 | digit(part[3])?);
            }
        }
        if output.len() > PAINT_LIMIT {
            return Err(());
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn base64_is_canonical_and_bounded() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(decode(encoded).unwrap(), plain.as_bytes());
        }
        for invalid in ["Zg", "Zh==", "Zm9=", "Zg==Zg==", "====", "Zm-v", "Zg==\n"] {
            assert!(decode(invalid).is_err(), "{invalid}");
        }
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        assert_eq!(
            decode(&encode(&vec![0; PAINT_LIMIT])).unwrap().len(),
            PAINT_LIMIT
        );
        assert!(decode(&encode(&vec![0; PAINT_LIMIT + 1])).is_err());
    }

    #[test]
    fn authoritative_browser_fixture_is_actual_serialization() {
        fn value(seq: u64, event: Event<'_>) -> Value {
            serde_json::from_slice(&Record::line(seq, event).unwrap()).unwrap()
        }
        let mut streams = vec![json!({"name":"reset-data-reset-ended","records":[
            value(1,Event::Reset{continuity:"fixture-continuity",cols:120,rows:40,bytes:"G1syShtbSEhlbGxv"}),
            value(2,Event::Data{bytes:"G1szMW1SZWQ="}),
            value(3,Event::Reset{continuity:"fixture-continuity",cols:40,rows:10,bytes:"G1tITmV3"}),
            value(4,Event::Closed{reason:Closed::Ended})]})];
        for reason in [Closed::Replaced, Closed::Missing] {
            let name = match reason {
                Closed::Ended => "ended",
                Closed::Replaced => "replaced",
                Closed::Missing => "missing",
            };
            streams.push(json!({"name":name,"records":[value(1,Event::Closed{reason})]}));
        }
        for reason in [
            Unavailable::Unsupported,
            Unavailable::Busy,
            Unavailable::SourceLost,
            Unavailable::InvalidSource,
            Unavailable::SlowViewer,
            Unavailable::ServerStopping,
        ] {
            let name = match reason {
                Unavailable::Unsupported => "unsupported",
                Unavailable::Busy => "busy",
                Unavailable::SourceLost => "source_lost",
                Unavailable::InvalidSource => "invalid_source",
                Unavailable::SlowViewer => "slow_viewer",
                Unavailable::ServerStopping => "server_stopping",
            };
            streams.push(json!({"name":name,"records":[value(1,Event::Unavailable{reason})]}));
        }
        let fixture = json!({"version":1,"limits":{"encoded_record":ENCODED_LIMIT,"decoded_paint":PAINT_LIMIT,"decoded_data":DATA_LIMIT,"pending_http":PENDING_LIMIT,"cols":512,"rows":256,"cells":65536},"streams":streams});
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/live_peek/streams.json"
        );
        if std::env::var_os("NAMEDROP_UPDATE_LIVE_FIXTURE").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
            )
            .unwrap();
        }
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap(),
            fixture
        );
    }
}
