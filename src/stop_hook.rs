use namedrop::{
    Clock, SystemClock,
    cli::RuntimeOptions,
    final_reply::{BoundOperation, FinalText, HostTurn, StopFacts, TurnTasks},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_INPUT_BYTES: u64 = 1024 * 1024;
/// How long the Stop hook may look for a held task's notice before it hands
/// the Stop to a worker anyway, well inside its own 250 ms.
const NOTICE_SCAN_BUDGET: Duration = Duration::from_millis(100);
/// How much of the transcript's end the Stop reads for the tasks its turn
/// started.
const TURN_SCAN_BYTES: u64 = 8 * 1024 * 1024;
/// How long the worker waits for Claude Code to record how every Stop hook
/// of this Stop ended. It writes that record as soon as the last hook exits.
const STOP_OUTCOME_WAIT: Duration = Duration::from_secs(3);

#[derive(Deserialize, Serialize)]
struct StopInput {
    hook_event_name: String,
    session_id: String,
    last_assistant_message: Option<String>,
    transcript_path: Option<PathBuf>,
    turn_id: Option<String>,
    prompt_id: Option<String>,
    #[serde(default)]
    background_tasks: Vec<BackgroundTask>,
}

#[derive(Deserialize, Serialize)]
struct BackgroundTask {
    #[serde(default)]
    id: String,
    status: String,
}

#[derive(Deserialize, Serialize)]
struct CapturedStop {
    input: StopInput,
    socket: String,
    ended_at_ms: u64,
    transcript_end: Option<u64>,
    /// Whether the session holds a claim this Stop may settle.
    settles: bool,
}

fn deadline(milliseconds: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
        std::process::exit(0);
    });
}

/// Hands a Stop to the detached worker when the session holds a claim the
/// prompt hook armed, and every Claude Code Stop, whose worker records the
/// turn's end for delivery.
pub fn run(state: &Path) {
    deadline(250);
    let ended_at_ms = SystemClock.now_unix_ms();
    let Some(input) = read_input(io::stdin().lock()) else {
        return;
    };
    let Ok(socket) = env::var("HERDR_SOCKET_PATH") else {
        return;
    };
    let names_notice = |tasks: &[String]| {
        let path = input
            .transcript_path
            .as_ref()
            .filter(|path| path.is_absolute())?;
        namedrop::final_reply::names_notice(path, tasks, TURN_SCAN_BYTES, NOTICE_SCAN_BUDGET)
    };
    let settles = namedrop::final_reply::stop_has_work(
        state,
        &socket,
        &input.session_id,
        input.turn().as_ref(),
        names_notice,
    );
    if !settles && !input.is_claude() {
        return;
    }
    let transcript_end = input.transcript_path.as_ref().and_then(|path| {
        if !path.is_absolute() {
            return None;
        }
        let metadata = fs::symlink_metadata(path).ok()?;
        metadata.is_file().then_some(metadata.len())
    });
    if settles && let Some(turn) = input.turn() {
        namedrop::final_reply::mark_stopped(state, &socket, &input.session_id, &turn, ended_at_ms);
    }
    let captured = CapturedStop {
        input,
        socket,
        ended_at_ms,
        transcript_end,
        settles,
    };
    let Ok(executable) = env::current_exe() else {
        return;
    };
    // The front end's short Herdr timeout is for its own deadline; a settle
    // runs the CLI's operation, which needs the CLI's.
    let Ok(mut child) = Command::new(executable)
        .env_remove("NAMEDROP_HERDR_TIMEOUT_MS")
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
    // The warning gets its own time, so a busy store can't keep the worker alive.
    deadline(12_000);
    if captured.settles {
        let (at_deadline, socket, session, turn, ended_at_ms) = (
            state.to_owned(),
            captured.socket.clone(),
            captured.input.session_id.clone(),
            captured.input.turn(),
            captured.ended_at_ms,
        );
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            namedrop::final_reply::warn_stop_timed_out(
                &at_deadline,
                &socket,
                &session,
                turn.as_ref(),
                ended_at_ms,
            );
            std::process::exit(0);
        });
    }
    if let (Some(_), Some(path), Some(from)) = (
        &captured.input.prompt_id,
        &captured.input.transcript_path,
        captured.transcript_end,
    ) && another_hook_blocked(path, from)
    {
        return;
    }
    // Only now is the turn known to be over: a Stop another hook blocked
    // continues it.
    if captured.input.is_claude() {
        namedrop::turn_phase::record(
            state,
            &captured.socket,
            &captured.input.session_id,
            namedrop::turn_phase::TurnEdge::Stop,
            captured.ended_at_ms,
        );
    }
    if captured.settles {
        settle_stop(state, &captured);
    }
    if captured.input.is_claude() {
        wake_waiting_mail(state, &captured);
    }
}

fn settle_stop(state: &Path, captured: &CapturedStop) {
    let running_tasks: Vec<String> = captured
        .input
        .background_tasks
        .iter()
        .filter(|task| matches!(task.status.as_str(), "running" | "pending"))
        .map(|task| task.id.clone())
        .collect();
    let turn = captured.input.turn_tasks(captured.transcript_end);
    let stop = StopFacts {
        session: captured.input.session_id.clone(),
        turn: captured.input.turn(),
        read_turn: turn.started.is_some(),
        // When the turn's start can't be read, every running task counts as
        // started by it, so a final message that may be a status is held.
        started_tasks: turn.started.unwrap_or_else(|| running_tasks.clone()),
        notified_tasks: turn.notified,
        paused_tasks: turn.paused,
        ended_tasks: turn.ended,
        running_tasks,
        text: captured
            .input
            .final_text(Some(captured.transcript_end.unwrap_or(0))),
    };
    namedrop::final_reply::settle_at_stop(
        state,
        &captured.socket,
        &stop,
        captured.ended_at_ms,
        &mut settle,
    );
}

/// Herdr sends no status change when a session ends its turn with background
/// agents still running, so the Stop starts the drain that mail for it waits on.
fn wake_waiting_mail(state: &Path, captured: &CapturedStop) {
    let Ok(space) = env::var("HERDR_WORKSPACE_ID") else {
        return;
    };
    if namedrop::mail_waits_for_session(state, &captured.socket, &space, &captured.input.session_id)
    {
        let _ = crate::schedule_mailbox_workers(state, &captured.socket, &space, false);
    }
}

/// Whether another Stop hook blocked this Stop, so Claude Code continues the
/// turn and the text this Stop carries is not its final message. Claude Code
/// records how the Stop's hooks ended after the transcript's end at the
/// Stop: a "Stop hook feedback" prompt when one blocked, then a
/// `stop_hook_summary`. The turn's next Stop settles the claim instead.
/// With no summary in time it settles as it would without the check.
fn another_hook_blocked(path: &Path, from: u64) -> bool {
    let until = Instant::now() + STOP_OUTCOME_WAIT;
    loop {
        let mut bytes = Vec::new();
        let read = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .and_then(|mut file| {
                file.seek(SeekFrom::Start(from))?;
                file.take(MAX_INPUT_BYTES).read_to_end(&mut bytes)
            });
        if read.is_err() {
            return false;
        }
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &bytes[..end]);
        for record in complete
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        {
            if record["type"] == "user"
                && record["isMeta"] == true
                && record["message"]["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with("Stop hook feedback:"))
            {
                return true;
            }
            if record["type"] == "system" && record["subtype"] == "stop_hook_summary" {
                return false;
            }
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn settle(bound: BoundOperation) -> Result<Value, String> {
    let options = RuntimeOptions {
        plugin_state: Some(bound.state),
        herdr_socket: Some(bound.socket),
        herdr_workspace: Some(bound.workspace),
        herdr_pane: Some(bound.pane),
    };
    match crate::run_bound_operation(bound.operation, bound.input, &options) {
        crate::Ran::Finished(result) => result,
        crate::Ran::Refused(error) => Err(error),
    }
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
    /// A Claude Code Stop carries a prompt id and no turn id.
    fn is_claude(&self) -> bool {
        self.prompt_id.is_some() && self.turn_id.is_none()
    }

    fn turn(&self) -> Option<HostTurn> {
        HostTurn::from_ids(self.prompt_id.as_deref(), self.turn_id.as_deref())
    }

    fn final_text(&self, transcript_end: Option<u64>) -> FinalText {
        match &self.last_assistant_message {
            Some(text) => FinalText::bounded(text),
            None => FinalText::bounded(&self.transcript_text(transcript_end).unwrap_or_default()),
        }
    }

    /// What this turn's transcript says about its background work, with no
    /// started tasks when the turn's start can't be read.
    fn turn_tasks(&self, transcript_end: Option<u64>) -> TurnTasks {
        let read = self.prompt_id.as_deref().and_then(|prompt_id| {
            let (bytes, whole) = self.transcript_tail(transcript_end, TURN_SCAN_BYTES)?;
            Some(namedrop::final_reply::turn_tasks(&bytes, whole, prompt_id))
        });
        read.unwrap_or_default()
    }

    /// Up to `limit` bytes of the transcript before `transcript_end`, from the
    /// first whole line, and whether they start at the transcript's start.
    fn transcript_tail(&self, transcript_end: Option<u64>, limit: u64) -> Option<(Vec<u8>, bool)> {
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
        let start = end.saturating_sub(limit);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.take(end - start).read_to_end(&mut bytes).ok()?;
        if start != 0 {
            let first = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |at| at + 1);
            bytes.drain(..first);
        }
        Some((bytes, start == 0))
    }

    fn transcript_text(&self, transcript_end: Option<u64>) -> Option<String> {
        let (bytes, _) = self.transcript_tail(transcript_end, MAX_INPUT_BYTES)?;
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok());
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
    use namedrop::TERMINAL_RELAY_MAX_CHARS as MAX_TEXT_CHARS;
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
            assert_eq!(input.final_text(None).text, expected);
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
            assert_eq!(bounded.final_text(None).text, "");
        }
    }

    #[test]
    fn native_stop_text_is_bounded_and_preserved_as_plain_text() {
        let text = "<script>質問?</script>".repeat(MAX_TEXT_CHARS);
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":text}),
        );
        assert_eq!(
            input.final_text(None).text,
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
        assert_eq!(
            input.final_text(None).text,
            "Would you prefer tea or coffee?"
        );
    }

    #[test]
    fn transcript_fallback_requires_the_codex_turn_and_ignores_later_turns() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/terminal-relay/codex-0.164.0-transcript.jsonl");
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"codex-session-id", "turn_id":"codex-turn-id", "transcript_path":fixture}),
        );
        assert_eq!(input.final_text(None).text, "Hi! What can I help with?");
        let other = StopInput {
            turn_id: Some("different-turn".into()),
            ..input
        };
        assert_eq!(other.final_text(None).text, "");
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
        assert_eq!(input.final_text(None).text, "");
        fs::write(&transcript, &first).unwrap();
        assert_eq!(input.final_text(None).text, "old answer");
        let captured_end = fs::metadata(&transcript).unwrap().len();
        let later = json!({"type":"assistant", "sessionId":"test", "message":{"content":[{"type":"text","text":"later turn"}]}});
        fs::write(&transcript, format!("{first}\n{later}\n")).unwrap();
        assert_eq!(input.final_text(Some(captured_end)).text, "old answer");
        assert_eq!(input.final_text(None).text, "later turn");
        fs::write(&transcript, first).unwrap();
        input.session_id = "other-session".into();
        assert_eq!(input.final_text(None).text, "");
        let link = root.join("link");
        std::os::unix::fs::symlink(&transcript, &link).unwrap();
        input.transcript_path = Some(link);
        assert_eq!(input.final_text(None).text, "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_native_text_does_not_fall_back_to_an_old_answer() {
        let input = parse(
            json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":"", "transcript_path":"/not/a/transcript"}),
        );
        assert_eq!(input.final_text(None).text, "");
    }
}
