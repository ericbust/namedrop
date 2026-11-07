use namedrop::{
    ActorKind, AgentSessionId, Clock, MutationAttribution, ProcessHerdr, StateNamespace,
    StateStore, SystemClock, TerminalRelayEnd, final_reply::HostTurn,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const MAX_INPUT_BYTES: u64 = 1024 * 1024;
const MAX_TEXT_CHARS: usize = 16_000;

#[derive(Deserialize, Serialize)]
struct StopInput {
    hook_event_name: String,
    session_id: String,
    last_assistant_message: Option<String>,
    transcript_path: Option<PathBuf>,
    turn_id: Option<String>,
    prompt_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct CapturedStop {
    input: StopInput,
    socket: String,
    ended_at_ms: u64,
    previous_end_ms: Option<u64>,
    transcript_end: Option<u64>,
}

fn deadline(milliseconds: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
        std::process::exit(0);
    });
}

pub fn run(state: &Path) {
    deadline(250);
    let ended_at_ms = SystemClock.now_unix_ms();
    let Some(input) = read_input(io::stdin().lock()) else {
        return;
    };
    let Ok(socket) = env::var("HERDR_SOCKET_PATH") else {
        return;
    };
    let Some(previous_end_ms) = record_boundary(state, &socket, &input, ended_at_ms) else {
        return;
    };
    let transcript_end = input
        .last_assistant_message
        .is_none()
        .then(|| {
            let path = input.transcript_path.as_ref()?;
            if !path.is_absolute() {
                return None;
            }
            let metadata = fs::symlink_metadata(path).ok()?;
            metadata.is_file().then_some(metadata.len())
        })
        .flatten();
    let captured = CapturedStop {
        input,
        socket,
        ended_at_ms,
        previous_end_ms,
        transcript_end,
    };
    let Ok(executable) = env::current_exe() else {
        return;
    };
    let Ok(mut child) = Command::new(executable)
        .args(["hook", "stop-worker", "--state-directory"])
        .arg(state)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = serde_json::to_writer(&mut stdin, &captured);
    }
}

pub fn run_worker(state: &Path) {
    deadline(3000);
    let mut bytes = Vec::new();
    if io::stdin()
        .lock()
        .take(MAX_INPUT_BYTES * 2)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return;
    }
    let Ok(captured) = serde_json::from_slice::<CapturedStop>(&bytes) else {
        return;
    };
    let text = captured
        .input
        .final_text(Some(captured.transcript_end.unwrap_or(0)));
    let herdr = ProcessHerdr::default().with_socket(Some(captured.socket.clone()));
    let Some((workspace, _)) =
        namedrop::locate_agent_session_pane(&herdr, &captured.input.session_id)
    else {
        return;
    };
    let Ok(Some(mut store)) = StateStore::open_for_terminal_relay(state) else {
        return;
    };
    let namespace = StateNamespace::for_space(captured.socket, workspace);
    let ending = TerminalRelayEnd {
        agent_session_id: AgentSessionId::new(captured.input.session_id),
        ended_at_ms: captured.ended_at_ms,
        previous_end_ms: Some(captured.previous_end_ms.unwrap_or(0)),
        text,
    };
    let attribution = MutationAttribution::new(ActorKind::Plugin, "namedrop", captured.ended_at_ms);
    let _ = store.transaction(|transaction| {
        transaction.relay_terminal_ending(&namespace, &ending, &attribution)
    });
}

#[derive(Deserialize, Serialize)]
struct StopBoundary {
    identity: String,
    ended_at_ms: u64,
}

fn hash(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// The latest boundary also marks its own ID. Retire it to an immutable marker
// before replacement so a crash between writes cannot make an old Stop reusable.
fn record_boundary(
    state: &Path,
    socket: &str,
    input: &StopInput,
    ended_at_ms: u64,
) -> Option<Option<u64>> {
    let identity = input.turn()?.as_str().to_owned();
    if !state.is_dir() {
        return None;
    }
    let session = hash(
        serde_json::to_vec(&(socket, &input.session_id))
            .ok()?
            .as_slice(),
    );
    let directory = state.join("terminal-relay-stops").join(session);
    fs::create_dir_all(&directory).ok()?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("lock"))
        .ok()?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let boundary_path = directory.join("latest.json");
    let previous: Option<StopBoundary> = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&boundary_path)
    {
        Ok(file) => {
            if !file.metadata().ok()?.is_file() {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 4096 {
                return None;
            }
            Some(serde_json::from_slice(&bytes).ok()?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    if previous
        .as_ref()
        .is_some_and(|last| last.identity == identity)
        || directory.join(&identity).try_exists().ok()?
    {
        return None;
    }
    let mark_seen = |id: &str| -> Option<()> {
        // IDs read from disk remain data, never path components.
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(id))
        {
            Ok(_) => Some(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Some(()),
            Err(_) => None,
        }
    };
    if previous
        .as_ref()
        .is_some_and(|last| last.ended_at_ms >= ended_at_ms)
    {
        mark_seen(&identity)?;
        return None;
    }
    if let Some(last) = &previous {
        mark_seen(&last.identity)?;
    }
    let boundary = StopBoundary {
        identity,
        ended_at_ms,
    };
    let temporary = directory.join("next.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&temporary)
        .ok()?;
    file.write_all(&serde_json::to_vec(&boundary).ok()?).ok()?;
    fs::rename(temporary, boundary_path).ok()?;
    Some(previous.map(|last| last.ended_at_ms))
}

fn read_input(input: impl Read) -> Option<StopInput> {
    let mut bytes = Vec::new();
    input
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return stop_identity_prefix(&bytes);
    }
    let input: StopInput = serde_json::from_slice(&bytes).ok()?;
    (input.hook_event_name == "Stop" && !input.session_id.trim().is_empty()).then_some(input)
}

fn stop_identity_prefix(bytes: &[u8]) -> Option<StopInput> {
    fn whitespace(bytes: &[u8], offset: &mut usize) {
        while bytes.get(*offset).is_some_and(u8::is_ascii_whitespace) {
            *offset += 1;
        }
    }
    fn value(bytes: &[u8], offset: &mut usize) -> Option<Value> {
        let mut values =
            serde_json::Deserializer::from_slice(bytes.get(*offset..)?).into_iter::<Value>();
        let result = values.next()?.ok()?;
        *offset += values.byte_offset();
        Some(result)
    }
    let mut offset = 0;
    whitespace(bytes, &mut offset);
    if bytes.get(offset) != Some(&b'{') {
        return None;
    }
    offset += 1;
    let mut fields = serde_json::Map::new();
    fields.insert(
        "last_assistant_message".to_owned(),
        Value::String(String::new()),
    );
    loop {
        whitespace(bytes, &mut offset);
        let Value::String(key) = value(bytes, &mut offset)? else {
            return None;
        };
        whitespace(bytes, &mut offset);
        if bytes.get(offset) != Some(&b':') {
            return None;
        }
        offset += 1;
        let item = value(bytes, &mut offset)?;
        if matches!(
            key.as_str(),
            "hook_event_name" | "session_id" | "prompt_id" | "turn_id"
        ) {
            fields.insert(key, item);
        }
        if let Ok(input) = serde_json::from_value::<StopInput>(Value::Object(fields.clone()))
            && input.turn().is_some()
            && input.hook_event_name == "Stop"
            && !input.session_id.trim().is_empty()
        {
            return Some(input);
        }
        whitespace(bytes, &mut offset);
        if bytes.get(offset) != Some(&b',') {
            return None;
        }
        offset += 1;
    }
}

impl StopInput {
    fn turn(&self) -> Option<HostTurn> {
        HostTurn::from_ids(self.prompt_id.as_deref(), self.turn_id.as_deref())
    }

    fn final_text(&self, transcript_end: Option<u64>) -> String {
        let text = match &self.last_assistant_message {
            Some(text) => text.clone(),
            None => self.transcript_text(transcript_end).unwrap_or_default(),
        };
        text.chars().take(MAX_TEXT_CHARS).collect()
    }

    fn transcript_text(&self, transcript_end: Option<u64>) -> Option<String> {
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
        let end = transcript_end.unwrap_or(metadata.len());
        if end > metadata.len() {
            return None;
        }
        let start = end.saturating_sub(MAX_INPUT_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(end - start).read_to_end(&mut bytes).ok()?;
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
            assert_eq!(input.final_text(None), expected);
        }
    }

    #[test]
    fn oversized_recorded_host_payloads_keep_identity_and_discard_text() {
        for fixture in [
            include_str!("../tests/fixtures/terminal-relay/claude-2.1.316-stop.json"),
            include_str!("../tests/fixtures/terminal-relay/codex-0.164.0-stop.json"),
        ] {
            let original = read_input(fixture.as_bytes()).unwrap();
            let oversized = fixture.replace(
                &serde_json::to_string(original.last_assistant_message.as_ref().unwrap()).unwrap(),
                &serde_json::to_string(&"x".repeat(MAX_INPUT_BYTES as usize + 100)).unwrap(),
            );
            let bounded = read_input(oversized.as_bytes()).unwrap();
            assert_eq!(bounded.session_id, original.session_id);
            assert_eq!(bounded.prompt_id, original.prompt_id);
            assert_eq!(bounded.turn_id, original.turn_id);
            assert_eq!(bounded.final_text(None), "");
        }
    }

    #[test]
    fn native_stop_text_is_bounded_and_preserved_as_plain_text() {
        let text = "<script>質問?</script>".repeat(MAX_TEXT_CHARS);
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":text}),
        );
        assert_eq!(
            input.final_text(None),
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
        assert_eq!(input.final_text(None), "Would you prefer tea or coffee?");
    }

    #[test]
    fn transcript_fallback_requires_the_codex_turn_and_ignores_later_turns() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/terminal-relay/codex-0.164.0-transcript.jsonl");
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"codex-session-id", "turn_id":"codex-turn-id", "transcript_path":fixture}),
        );
        assert_eq!(input.final_text(None), "Hi! What can I help with?");
        let other = StopInput {
            turn_id: Some("different-turn".into()),
            ..input
        };
        assert_eq!(other.final_text(None), "");
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
        assert_eq!(input.final_text(None), "");
        fs::write(&transcript, &first).unwrap();
        assert_eq!(input.final_text(None), "old answer");
        let captured_end = fs::metadata(&transcript).unwrap().len();
        let later = json!({"type":"assistant", "sessionId":"test", "message":{"content":[{"type":"text","text":"later turn"}]}});
        fs::write(&transcript, format!("{first}\n{later}\n")).unwrap();
        assert_eq!(input.final_text(Some(captured_end)), "old answer");
        assert_eq!(input.final_text(None), "later turn");
        fs::write(&transcript, first).unwrap();
        input.session_id = "other-session".into();
        assert_eq!(input.final_text(None), "");
        let link = root.join("link");
        std::os::unix::fs::symlink(&transcript, &link).unwrap();
        input.transcript_path = Some(link);
        assert_eq!(input.final_text(None), "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_native_text_does_not_fall_back_to_an_old_answer() {
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":"", "transcript_path":"/not/a/transcript"}),
        );
        assert_eq!(input.final_text(None), "");
    }
}
