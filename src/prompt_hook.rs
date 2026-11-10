//! The turn start a Claude Code prompt records, and the worker that learns
//! whether the prompt's hooks let it start the turn.

use crate::stop_hook::{HOOK_OUTCOME_WAIT, deadline, records_after, wake_waiting_mail};
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
    prompt: Option<String>,
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

/// What the hook hands the worker: the prompt and its text, and where in the
/// transcript Claude Code will record how the prompt's hooks ended.
#[derive(Deserialize, Serialize)]
struct Watched {
    prompted: Prompted,
    prompt: String,
    transcript_path: PathBuf,
    transcript_end: u64,
}

/// Records that a Claude Code prompt starts a turn, from the hook's input and
/// environment alone, before anything asks Herdr: a Herdr error must not leave
/// the last Stop newest while the turn runs. A Claude Code prompt carries a
/// prompt id and no turn id. Another hook may still block or stop the prompt,
/// so a worker watches the transcript for that.
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
    if let Some(prompt) = input.prompt
        && let Some(transcript_path) = input.transcript_path.filter(|path| path.is_absolute())
        && let Some(transcript_end) = fs::symlink_metadata(&transcript_path)
            .ok()
            .filter(fs::Metadata::is_file)
            .map(|metadata| metadata.len())
    {
        watch(
            state,
            &Watched {
                prompted: prompted.clone(),
                prompt,
                transcript_path,
                transcript_end,
            },
        );
    }
    Some(prompted)
}

impl Prompted {
    /// A hook blocked or stopped the prompt, so it started no turn.
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

/// Whether Claude Code ended the prompt without a turn, from the records it
/// wrote after the prompt's hooks ran. A hook that stops the prompt leaves it
/// recorded under its id, with the ending after it. A hook that blocks it
/// leaves only an ending, which names the prompt by its text alone; another
/// prompt with the same text may be the one blocked, so that ending counts
/// only while the prompt's own record stays out of the transcript.
fn ended_without_turn(records: &[Value], prompt_id: &str, prompt: &str) -> bool {
    let own: Vec<&Value> = records
        .iter()
        .filter(|record| record["promptId"] == prompt_id)
        .collect();
    let mut endings = records
        .iter()
        .filter(|record| record["promptSubmitEnding"].is_object());
    if own.is_empty() {
        let suffix = format!("\n\nOriginal prompt: {prompt}");
        endings.any(|ending| {
            ending["content"]
                .as_str()
                .is_some_and(|content| content.ends_with(&suffix))
        })
    } else {
        endings.any(|ending| {
            own.iter()
                .any(|record| record["uuid"].is_string() && record["uuid"] == ending["parentUuid"])
        })
    }
}

/// Waits for Claude Code to record how the prompt's hooks ended. A prompt
/// another hook blocked or stopped started no turn, so the session is back
/// where it was and mail that waited for its turn may go in. With no ending
/// in time the turn counts as started.
pub fn run_worker(state: &Path) {
    let mut bytes = Vec::new();
    if io::stdin()
        .lock()
        .take(2 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return;
    }
    let Ok(watched) = serde_json::from_slice::<Watched>(&bytes) else {
        return;
    };
    deadline(5_000);
    // The prompt's own record may still follow a block of the same text.
    std::thread::sleep(HOOK_OUTCOME_WAIT);
    let prompted = &watched.prompted;
    if records_after(&watched.transcript_path, watched.transcript_end)
        .is_some_and(|records| ended_without_turn(&records, &prompted.prompt_id, &watched.prompt))
    {
        prompted.blocked(state);
        wake_waiting_mail(state, &prompted.socket, &prompted.session);
    }
}
