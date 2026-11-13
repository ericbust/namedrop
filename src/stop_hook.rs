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
    let Some(socket) = namedrop::locator::Locator::Endpoint.env() else {
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
    // The worker stays in the hook's process group, and its endpoint and space
    // travel in `CapturedStop`. The scrub also drops the front end's short
    // timeouts, which are for its own deadline, not a settle's.
    let mut command = Command::new(executable);
    namedrop::daemon::scrub(&mut command);
    let Ok(mut child) = command
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
    let outcome = match (
        &captured.input.prompt_id,
        &captured.input.transcript_path,
        captured.transcript_end,
    ) {
        (Some(_), Some(path), Some(from)) => stop_hook_outcome(path, from),
        _ => None,
    };
    if outcome == Some(StopOutcome::Continued) {
        return;
    }
    // Only now is the turn known to be over: a Stop another hook blocked
    // continues it. A hook slower than the wait may still block, so with no
    // outcome the turn counts as running, and its mail waits on Herdr's status
    // or the turn's next Stop.
    let ended = captured.input.is_claude() && outcome == Some(StopOutcome::Ended);
    if ended {
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
    if ended {
        wake_waiting_mail(state, &captured.socket, &captured.input.session_id);
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
/// agents still running, so the Stop's worker starts the drain that mail for
/// the session waits on.
fn wake_waiting_mail(state: &Path, socket: &str, session: &str) {
    let Some(space) = namedrop::locator::Locator::Space.env() else {
        return;
    };
    if namedrop::mail_waits_for_session(state, socket, &space, session) {
        let _ = crate::schedule_mailbox_workers(state, socket, &space, false);
    }
}

/// How a Stop's hooks ended. Claude Code records it after the transcript's
/// end at the Stop: a "Stop hook feedback" prompt when one blocked, then a
/// `stop_hook_summary`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopOutcome {
    Ended,
    /// Another Stop hook blocked this Stop, so Claude Code continues the turn
    /// and the text this Stop carries is not its final message. The turn's
    /// next Stop settles the claim instead.
    Continued,
}

fn stop_outcome(record: &Value) -> Option<StopOutcome> {
    if record["type"] == "user"
        && record["isMeta"] == true
        && record["message"]["content"]
            .as_str()
            .is_some_and(|content| content.starts_with("Stop hook feedback:"))
    {
        return Some(StopOutcome::Continued);
    }
    (record["type"] == "system" && record["subtype"] == "stop_hook_summary")
        .then_some(StopOutcome::Ended)
}

/// The first outcome Claude Code records after `from`, or `None` when it
/// records none in time or the transcript can't be read.
fn stop_hook_outcome(path: &Path, from: u64) -> Option<StopOutcome> {
    let until = Instant::now() + STOP_OUTCOME_WAIT;
    loop {
        let mut bytes = Vec::new();
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .and_then(|mut file| {
                file.seek(SeekFrom::Start(from))?;
                file.take(MAX_INPUT_BYTES).read_to_end(&mut bytes)
            })
            .ok()?;
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &bytes[..end]);
        if let Some(found) = complete
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .find_map(|record| stop_outcome(&record))
        {
            return Some(found);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn settle(bound: BoundOperation) -> Result<Value, String> {
    let options = RuntimeOptions {
        plugin_state: Some(bound.state),
        endpoint: Some(bound.socket),
        space: Some(bound.workspace),
        pane: Some(bound.pane),
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
            "hook_event_name" | "session_id" | "prompt_id" | "turn_id" | "transcript_path"
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
            // Kept so the worker can still learn whether the turn ended.
            assert_eq!(bounded.transcript_path, original.transcript_path);
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
