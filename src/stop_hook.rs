use namedrop::{
    ActorKind, AgentSessionId, Clock, MutationAttribution, ProcessHerdr, StateNamespace,
    StateStore, SystemClock, TerminalRelayEnd,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const MAX_INPUT_BYTES: u64 = 1024 * 1024;
const MAX_TEXT_CHARS: usize = 16_000;

#[derive(Deserialize)]
struct StopInput {
    hook_event_name: String,
    session_id: String,
    last_assistant_message: Option<String>,
    transcript_path: Option<PathBuf>,
    turn_id: Option<String>,
}

pub fn run(state: &Path) {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        std::process::exit(0);
    });
    let ended_at_ms = SystemClock.now_unix_ms();
    let Some(input) = read_input(io::stdin().lock()) else {
        return;
    };
    let text = input.final_text();
    let Ok(socket) = env::var("HERDR_SOCKET_PATH") else {
        return;
    };
    let herdr = ProcessHerdr::default().with_socket(Some(socket.clone()));
    let Some((workspace, _)) = namedrop::locate_agent_session_pane(&herdr, &input.session_id)
    else {
        return;
    };
    let Ok(Some(mut store)) = StateStore::open_for_terminal_relay(state) else {
        return;
    };
    let namespace = StateNamespace::for_space(socket, workspace);
    let ending = TerminalRelayEnd {
        agent_session_id: AgentSessionId::new(input.session_id),
        ended_at_ms,
        text,
    };
    let attribution = MutationAttribution::new(ActorKind::Plugin, "namedrop", ended_at_ms);
    let _ = store.transaction(|transaction| {
        transaction.relay_terminal_ending(&namespace, &ending, &attribution)
    });
}

fn read_input(input: impl Read) -> Option<StopInput> {
    let mut bytes = Vec::new();
    input
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return None;
    }
    let input: StopInput = serde_json::from_slice(&bytes).ok()?;
    (input.hook_event_name == "Stop" && !input.session_id.trim().is_empty()).then_some(input)
}

impl StopInput {
    fn final_text(&self) -> String {
        let text = match &self.last_assistant_message {
            Some(text) => text.clone(),
            None => self.transcript_text().unwrap_or_default(),
        };
        text.chars().take(MAX_TEXT_CHARS).collect()
    }

    fn transcript_text(&self) -> Option<String> {
        let path = self.transcript_path.as_ref()?;
        if !path.is_absolute() {
            return None;
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        let start = metadata.len().saturating_sub(MAX_INPUT_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_INPUT_BYTES).read_to_end(&mut bytes).ok()?;
        let mut lines = bytes.split(|byte| *byte == b'\n');
        if start != 0 {
            lines.next();
        }
        let records = lines.filter_map(|line| serde_json::from_slice::<Value>(line).ok());
        let mut text = None;
        for record in records {
            if record["type"] == "user" {
                text = None;
            }
            if record["type"] == "assistant" && record["sessionId"] == self.session_id {
                text = content_text(&record["message"]["content"], "text");
            }
            if let Some(turn_id) = self.turn_id.as_deref() {
                let message = &record["payload"];
                if record["type"] == "response_item"
                    && message["type"] == "message"
                    && message["role"] == "assistant"
                    && message["phase"] == "final_answer"
                    && message["internal_chat_message_metadata_passthrough"]["turn_id"] == turn_id
                {
                    text = content_text(&message["content"], "output_text");
                }
                if record["type"] == "event_msg"
                    && message["type"] == "task_complete"
                    && message["turn_id"] == turn_id
                {
                    text = message["last_agent_message"].as_str().map(str::to_owned);
                }
            }
        }
        text
    }
}

fn content_text(content: &Value, kind: &str) -> Option<String> {
    let text = content
        .as_array()?
        .iter()
        .filter(|item| item["type"] == kind)
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> StopInput {
        read_input(value.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn recorded_host_stop_payloads_supply_the_final_text() {
        for (fixture, expected) in [
            (
                include_str!("../tests/fixtures/terminal-relay/claude-2.1.316-stop.json"),
                "Hi! I'm @claude in this Namedrop testbed. What can I help you with?",
            ),
            (
                include_str!("../tests/fixtures/terminal-relay/codex-0.164.0-stop.json"),
                "Hi! What can I help with?",
            ),
        ] {
            let input = read_input(fixture.as_bytes()).unwrap();
            assert_eq!(input.final_text(), expected);
        }
    }

    #[test]
    fn native_stop_text_is_bounded_and_preserved_as_plain_text() {
        let text = "<script>質問?</script>".repeat(MAX_TEXT_CHARS);
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":text}),
        );
        assert_eq!(
            input.final_text(),
            text.chars().take(MAX_TEXT_CHARS).collect::<String>()
        );
    }

    #[test]
    fn rejects_other_events_missing_identity_and_oversized_payloads() {
        for value in [
            json!({"hook_event_name":"SubagentStop", "session_id":"session"}),
            json!({"hook_event_name":"Stop", "session_id":" "}),
            json!({"hook_event_name":"Stop"}),
        ] {
            assert!(read_input(value.to_string().as_bytes()).is_none());
        }
        assert!(read_input(vec![b' '; MAX_INPUT_BYTES as usize + 1].as_slice()).is_none());
    }

    #[test]
    fn recorded_claude_transcript_supplies_only_the_last_assistant_text() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/terminal-relay/claude-2.1.316-transcript.jsonl");
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"test-session", "transcript_path":fixture}),
        );
        assert_eq!(input.final_text(), "Would you prefer tea or coffee?");
    }

    #[test]
    fn transcript_fallback_requires_the_codex_turn_and_ignores_later_turns() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/terminal-relay/codex-0.164.0-transcript.jsonl");
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"codex-session-id", "turn_id":"codex-turn-id", "transcript_path":fixture}),
        );
        assert_eq!(input.final_text(), "Hi! What can I help with?");
        let other = StopInput {
            turn_id: Some("different-turn".into()),
            ..input
        };
        assert_eq!(other.final_text(), "");
    }

    #[test]
    fn transcript_reads_are_bounded_and_reject_symlinks() {
        let root = env::temp_dir().join(format!("namedrop-stop-tail-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let transcript = root.join("transcript.jsonl");
        let first = json!({"type":"assistant", "sessionId":"test", "message":{"content":[{"type":"text","text":"old answer"}]}}).to_string();
        fs::write(
            &transcript,
            format!("{first}\n{}\n", " ".repeat(MAX_INPUT_BYTES as usize)),
        )
        .unwrap();
        let mut input = parse(
            json!({"hook_event_name":"Stop", "session_id":"test", "transcript_path":transcript}),
        );
        assert_eq!(input.final_text(), "");
        fs::write(&transcript, first).unwrap();
        assert_eq!(input.final_text(), "old answer");
        input.session_id = "other-session".into();
        assert_eq!(input.final_text(), "");
        let link = root.join("link");
        std::os::unix::fs::symlink(&transcript, &link).unwrap();
        input.transcript_path = Some(link);
        assert_eq!(input.final_text(), "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_native_text_does_not_fall_back_to_an_old_answer() {
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":"", "transcript_path":"/not/a/transcript"}),
        );
        assert_eq!(input.final_text(), "");
    }
}
