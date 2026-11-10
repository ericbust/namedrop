//! The turn start a Claude Code prompt records, and the worker that learns
//! whether the prompt's hooks let it start the turn.

use crate::stop_hook::{deadline, hook_outcome, wake_waiting_mail};
use namedrop::{
    Clock, SystemClock,
    turn_phase::{self, TurnEdge},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Deserialize)]
struct PromptInput {
    hook_event_name: String,
    session_id: Option<String>,
    prompt_id: Option<String>,
    turn_id: Option<String>,
    transcript_path: Option<PathBuf>,
}

/// A Claude Code prompt whose turn start this hook recorded.
#[derive(Clone, Deserialize, Serialize)]
pub struct Prompted {
    socket: String,
    session: String,
    prompt_id: String,
    at_ms: u64,
}

/// What the hook hands the worker: the prompt, and where in the transcript
/// Claude Code will record how the prompt's hooks ended.
#[derive(Deserialize, Serialize)]
struct Watched {
    prompted: Prompted,
    transcript_path: PathBuf,
    transcript_end: u64,
}

/// Records that a Claude Code prompt starts a turn, from the hook's input and
/// environment alone, before anything asks Herdr: a Herdr error must not leave
/// the last Stop newest while the turn runs. A Claude Code prompt carries a
/// prompt id and no turn id. Another hook may still block the prompt, so a
/// worker watches the transcript for that.
pub fn record(state: &Path, input: &str) -> Option<Prompted> {
    let input: PromptInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name != "UserPromptSubmit" || input.turn_id.is_some() {
        return None;
    }
    let prompted = Prompted {
        socket: env::var("HERDR_SOCKET_PATH").ok()?,
        session: input.session_id.filter(|id| !id.is_empty())?,
        prompt_id: input.prompt_id?,
        at_ms: SystemClock.now_unix_ms(),
    };
    turn_phase::record(
        state,
        &prompted.socket,
        &prompted.session,
        TurnEdge::Prompt,
        prompted.at_ms,
    );
    if let Some(transcript_path) = input.transcript_path.filter(|path| path.is_absolute())
        && let Some(transcript_end) = fs::symlink_metadata(&transcript_path)
            .ok()
            .filter(fs::Metadata::is_file)
            .map(|metadata| metadata.len())
    {
        watch(
            state,
            &Watched {
                prompted: prompted.clone(),
                transcript_path,
                transcript_end,
            },
        );
    }
    Some(prompted)
}

impl Prompted {
    /// A hook blocked the prompt, so it started no turn.
    pub fn blocked(&self, state: &Path) {
        turn_phase::record(
            state,
            &self.socket,
            &self.session,
            TurnEdge::Blocked,
            self.at_ms,
        );
    }
}

fn watch(state: &Path, watched: &Watched) {
    let Ok(executable) = env::current_exe() else {
        return;
    };
    let Ok(mut child) = Command::new(executable)
        .args(["hook", "prompt-worker", "--state-directory"])
        .arg(state)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = serde_json::to_writer(&mut stdin, watched);
    }
}

/// How a prompt's hooks ended, as Claude Code records it after they all ran.
#[derive(Clone, Copy, Eq, PartialEq)]
enum PromptOutcome {
    /// The prompt is in the transcript: its turn started.
    Started,
    /// A hook blocked it, and Claude Code started no turn.
    Blocked,
}

fn prompt_outcome(record: &Value, prompt_id: &str) -> Option<PromptOutcome> {
    if record["type"] == "user" && record["promptId"] == prompt_id {
        return Some(PromptOutcome::Started);
    }
    (record["promptSubmitEnding"]["ending"]["kind"] == "blocked_by_hook")
        .then_some(PromptOutcome::Blocked)
}

/// Waits for Claude Code to record how the prompt's hooks ended. A prompt
/// another hook blocked started no turn, so the session is back where it was
/// and mail that waited for its turn may go in. With no outcome in time the
/// turn counts as started.
pub fn run_worker(state: &Path) {
    let mut bytes = Vec::new();
    if io::stdin()
        .lock()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return;
    }
    let Ok(watched) = serde_json::from_slice::<Watched>(&bytes) else {
        return;
    };
    deadline(5_000);
    let prompted = &watched.prompted;
    if hook_outcome(&watched.transcript_path, watched.transcript_end, |record| {
        prompt_outcome(record, &prompted.prompt_id)
    }) == Some(PromptOutcome::Blocked)
    {
        prompted.blocked(state);
        wake_waiting_mail(state, &prompted.socket, &prompted.session);
    }
}
