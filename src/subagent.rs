//! Host subagent events and independent file facts consumed by Channels.
use crate::{AgentRuntime, AgentSessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const SUBAGENT_TEXT_MAX: usize = 80;
/// A Codex rollout's first line carries the base instructions (about 23 KiB
/// today), so the cap leaves room for custom instructions.
pub const HOST_FILE_CAP: usize = 256 * 1024;
pub const SUBAGENT_STDIN_CAP: usize = 1024 * 1024;
const ID_MAX: usize = 128;

/// Host-issued id, safe to splice into a file name and a DOM key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentId(String);

impl SubagentId {
    pub fn parse(raw: &str) -> Option<Self> {
        is_token(raw).then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_token(raw: &str) -> bool {
    (1..=ID_MAX).contains(&raw.len())
        && raw != "."
        && raw != ".."
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Short host-authored label: no control or invisible formatting characters,
/// whitespace collapsed, capped, never empty. Redaction still happens once,
/// at the snapshot, like every other collaborator string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentText(String);

impl SubagentText {
    pub fn parse(raw: &str) -> Option<Self> {
        let cleaned: String = raw
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .filter(|c| !crate::domain::is_invisible_format(*c))
            .collect();
        let text: String = cleaned
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(SUBAGENT_TEXT_MAX)
            .collect();
        (!text.is_empty()).then_some(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubagentEdge {
    Start,
    Stop,
}

/// One host hook event, parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentSighting {
    pub id: SubagentId,
    /// The parent agent's session as the host asserts it.
    pub parent_session: Option<AgentSessionId>,
    pub edge: SubagentEdge,
    pub kind: Option<SubagentText>,
    pub name: Option<SubagentText>,
}

/// Bounded host file access, a seam so parsing stays testable without disk.
pub trait HostFiles {
    /// A regular file's first line, or its whole content, when it fits in
    /// `cap` bytes. `None` on any error, symlink, or non-regular file.
    fn read_head(&self, path: &Path, cap: usize, first_line_only: bool) -> Option<String>;
}

pub struct DiskFiles;

impl HostFiles for DiskFiles {
    fn read_head(&self, path: &Path, cap: usize, first_line_only: bool) -> Option<String> {
        if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
            return None;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let mut limited = BufReader::new(file).take(cap as u64 + 1);
        let mut bytes = Vec::new();
        if first_line_only {
            limited.read_until(b'\n', &mut bytes).ok()?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
        } else {
            limited.read_to_end(&mut bytes).ok()?;
        }
        if bytes.len() > cap {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}

#[derive(Deserialize)]
struct RawPayload {
    hook_event_name: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    agent_type: Option<String>,
    transcript_path: Option<PathBuf>,
    agent_transcript_path: Option<PathBuf>,
    #[serde(default)]
    background_tasks: Value,
    description: Option<String>,
}

impl SubagentSighting {
    /// `None` for any other event, bad JSON, or an invalid id. Never waits for
    /// a file: a name that is not readable now is simply absent.
    pub fn parse(host: AgentRuntime, stdin: &str, files: &impl HostFiles) -> Option<Self> {
        let raw: RawPayload = serde_json::from_str(stdin).ok()?;
        let edge = match raw.hook_event_name.as_str() {
            "SubagentStart" => SubagentEdge::Start,
            "SubagentStop" => SubagentEdge::Stop,
            _ => return None,
        };
        let id = SubagentId::parse(raw.agent_id.as_deref()?)?;
        let parent_session = raw
            .session_id
            .as_deref()
            .filter(|session| is_token(session))
            .map(AgentSessionId::new);
        let declared_kind = raw.agent_type.as_deref().and_then(SubagentText::parse);
        let (kind, name) = match (host, edge) {
            (AgentRuntime::ClaudeCode, SubagentEdge::Start) => (declared_kind, None),
            (AgentRuntime::ClaudeCode, SubagentEdge::Stop) => {
                let (meta_kind, meta_name) = raw
                    .agent_transcript_path
                    .as_deref()
                    .and_then(|path| claude_meta_path(path, &id))
                    .and_then(|path| files.read_head(&path, HOST_FILE_CAP, false))
                    .map(|text| claude_meta(&text))
                    .unwrap_or_default();
                let task_name = raw
                    .background_tasks
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|task| task["id"].as_str() == Some(id.as_str()))
                    .and_then(|task| task["description"].as_str())
                    .and_then(SubagentText::parse);
                (meta_kind.or(declared_kind), task_name.or(meta_name))
            }
            (AgentRuntime::Codex, edge) => {
                let rollout = match edge {
                    SubagentEdge::Start => raw.transcript_path.as_deref(),
                    SubagentEdge::Stop => raw.agent_transcript_path.as_deref(),
                };
                let name = rollout
                    .and_then(|path| files.read_head(path, HOST_FILE_CAP, true))
                    .and_then(|line| codex_spawn_name(&line));
                let kind = declared_kind.filter(|kind| kind.as_str() != "default");
                (kind, name)
            }
        };
        Some(Self {
            id,
            parent_session,
            edge,
            kind,
            name: raw
                .description
                .as_deref()
                .and_then(SubagentText::parse)
                .or(name),
        })
    }
}

/// The sibling meta file, built from the validated id; only a payload path
/// that is this subagent's own transcript is trusted for its directory.
fn claude_meta_path(agent_transcript_path: &Path, id: &SubagentId) -> Option<PathBuf> {
    let file_name = format!("agent-{}.jsonl", id.as_str());
    let directory = agent_transcript_path.parent()?;
    (agent_transcript_path.file_name()?.to_str()? == file_name
        && directory.file_name()?.to_str()? == "subagents"
        && agent_transcript_path.is_absolute())
    .then(|| directory.join(format!("agent-{}.meta.json", id.as_str())))
}

fn claude_meta(text: &str) -> (Option<SubagentText>, Option<SubagentText>) {
    let Ok(meta) = serde_json::from_str::<Value>(text) else {
        return (None, None);
    };
    let field = |key: &str| meta.get(key)?.as_str().and_then(SubagentText::parse);
    (field("agentType"), field("description"))
}

/// "say_hi (Aristotle)" from a child rollout's `session_meta` line.
fn codex_spawn_name(first_line: &str) -> Option<SubagentText> {
    let line: Value = serde_json::from_str(first_line).ok()?;
    if line.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let spawn = line.pointer("/payload/source/subagent/thread_spawn")?;
    let task = spawn
        .get("agent_path")
        .and_then(Value::as_str)
        .and_then(|path| path.rsplit('/').find(|segment| !segment.is_empty()));
    let nickname = spawn.get("agent_nickname").and_then(Value::as_str);
    let label = match (task, nickname) {
        (Some(task), Some(nickname)) => format!("{task} ({nickname})"),
        (Some(task), None) => task.to_owned(),
        (None, Some(nickname)) => nickname.to_owned(),
        (None, None) => return None,
    };
    SubagentText::parse(&label)
}

pub const MAX_RUN_AGE_MS: i64 = 2 * 60 * 60 * 1000;
const FACT_CAP: u64 = 16 * 1024;
const FACTS_PER_PARENT: usize = 4096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParentIdentity {
    pub socket: String,
    pub workspace: String,
    pub pane: String,
    pub session: String,
}

impl ParentIdentity {
    pub fn new(socket: &str, workspace: &str, pane: &str, session: &str) -> Option<Self> {
        [socket, workspace, pane, session]
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 4096 && !s.contains('\0'))
            .then(|| Self {
                socket: socket.to_owned(),
                workspace: workspace.to_owned(),
                pane: pane.to_owned(),
                session: session.to_owned(),
            })
    }

    fn directory(&self, state: &Path) -> PathBuf {
        let mut digest = Sha256::new();
        for part in [&self.socket, &self.workspace, &self.pane, &self.session] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part.as_bytes());
        }
        state
            .join("subagents/v1")
            .join(crate::lower_hex(&digest.finalize()))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Event {
    Start {
        id: String,
        #[serde(default)]
        generation: Option<String>,
        kind: Option<String>,
        description: Option<String>,
    },
    Stop {
        id: String,
        #[serde(default)]
        generation: Option<String>,
    },
    ParentStop {
        background: Vec<String>,
        #[serde(default)]
        generation: Option<String>,
    },
    ParentEnd,
    Reset,
    Resume,
}

impl Event {
    pub fn parse(
        host: AgentRuntime,
        input: &str,
        files: &impl HostFiles,
    ) -> Option<(String, Self)> {
        let value: Value = serde_json::from_str(input).ok()?;
        let session = value["session_id"]
            .as_str()
            .filter(|s| is_token(s))?
            .to_owned();
        let generation = value["prompt_id"]
            .as_str()
            .or_else(|| value["turn_id"].as_str())
            .filter(|value| is_token(value))
            .map(str::to_owned);
        let event = match value["hook_event_name"].as_str()? {
            "SubagentStop" => Self::Stop {
                id: SubagentId::parse(value["agent_id"].as_str()?)?.0,
                generation,
            },
            "SubagentStart" => {
                let sighting = SubagentSighting::parse(host, input, files)?;
                Self::Start {
                    id: sighting.id.0,
                    generation,
                    kind: sighting.kind.map(|text| text.0),
                    description: sighting.name.map(|text| text.0),
                }
            }
            "Stop" => Self::ParentStop {
                generation,
                background: value["background_tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|task| {
                        task["status"]
                            .as_str()
                            .is_some_and(|s| matches!(s, "running" | "pending"))
                    })
                    .filter_map(|task| {
                        task["id"]
                            .as_str()
                            .filter(|id| is_token(id))
                            .map(str::to_owned)
                    })
                    .take(256)
                    .collect(),
            },
            "SessionEnd" => Self::ParentEnd,
            "SessionStart"
                if value["source"]
                    .as_str()
                    .is_some_and(|source| matches!(source, "resume" | "compact")) =>
            {
                Self::Resume
            }
            "SessionStart" => Self::Reset,
            "UserPromptSubmit" => Self::Resume,
            _ => return None,
        };
        Some((session, event))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Fact {
    parent: ParentIdentity,
    at_ms: i64,
    event: Event,
}

#[derive(Debug)]
pub struct Running {
    pub id: String,
    pub kind: Option<String>,
    pub description: Option<String>,
    pub started_at_ms: i64,
}

pub fn record(
    state: &Path,
    parent: &ParentIdentity,
    event: Event,
    at_ms: i64,
) -> std::io::Result<()> {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let directory = parent.directory(state);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let name = match &event {
        Event::Start { id, generation, .. } => format!(
            "{}.start.json",
            hash_id(&format!(
                "{id}\0{}",
                generation.as_deref().unwrap_or_default()
            ))
        ),
        Event::Stop { id, generation } => format!(
            "{}.stop.json",
            hash_id(&format!(
                "{id}\0{}",
                generation.as_deref().unwrap_or_default()
            ))
        ),
        _ => format!(
            "parent-{at_ms}-{}-{}.json",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let temporary = directory.join(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let bytes = serde_json::to_vec(&Fact {
            parent: parent.clone(),
            at_ms,
            event,
        })?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        match std::fs::hard_link(&temporary, directory.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn hash_id(id: &str) -> String {
    crate::lower_hex(&Sha256::digest(id.as_bytes()))
}

fn facts(directory: &Path) -> Vec<Fact> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.take(FACTS_PER_PARENT + 1).collect();
    if entries.len() > FACTS_PER_PARENT {
        return Vec::new();
    }
    let mut output = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return Vec::new() };
        if !entry.path().extension().is_some_and(|ext| ext == "json") {
            continue;
        }
        let parsed = (|| {
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(entry.path())
                .ok()?;
            let mut bytes = Vec::new();
            file.take(FACT_CAP + 1).read_to_end(&mut bytes).ok()?;
            if bytes.len() > FACT_CAP as usize {
                return None;
            }
            serde_json::from_slice(&bytes).ok()
        })();
        let Some(fact) = parsed else {
            return Vec::new();
        };
        output.push(fact);
    }
    output
}

/// `idle_at_ms` is an authoritative native status observation, never derived
/// from delivery or assignment activity. Persist it before showing the runs.
pub fn running(
    state: &Path,
    parent: &ParentIdentity,
    now_ms: i64,
    idle_at_ms: Option<i64>,
) -> Vec<Running> {
    use std::collections::HashSet;
    let directory = parent.directory(state);
    let facts: Vec<_> = facts(&directory)
        .into_iter()
        .filter(|fact| &fact.parent == parent)
        .collect();
    let ended = facts
        .iter()
        .any(|fact| matches!(fact.event, Event::ParentEnd));
    if ended {
        return Vec::new();
    }
    let reset = facts
        .iter()
        .filter(|f| matches!(f.event, Event::Reset))
        .map(|f| f.at_ms)
        .max()
        .unwrap_or(i64::MIN);
    let latest_stop = facts
        .iter()
        .filter(|f| matches!(f.event, Event::ParentStop { .. }))
        .max_by_key(|f| f.at_ms);
    let resume = facts
        .iter()
        .filter(|f| matches!(f.event, Event::Resume))
        .map(|f| f.at_ms)
        .max()
        .unwrap_or(i64::MIN);
    let stopped: HashSet<_> = facts
        .iter()
        .filter_map(|f| match &f.event {
            Event::Stop { id, generation } => Some((id.as_str(), generation.as_deref())),
            _ => None,
        })
        .collect();
    let mut runs = Vec::new();
    for fact in &facts {
        let Event::Start {
            id,
            generation,
            kind,
            description,
        } = &fact.event
        else {
            continue;
        };
        if !is_token(id)
            || stopped.contains(&(id.as_str(), None))
            || stopped.contains(&(id.as_str(), generation.as_deref()))
            || fact.at_ms < reset
            || fact.at_ms > now_ms
            || now_ms.saturating_sub(fact.at_ms) >= MAX_RUN_AGE_MS
        {
            continue;
        }
        let background = latest_stop.is_some_and(|stop| match &stop.event {
            Event::ParentStop { background, .. } => background.contains(id),
            _ => false,
        });
        let turn_stopped = generation.is_some()
            && facts.iter().any(|fact| {
                matches!(&fact.event, Event::ParentStop { generation: turn, background }
                if turn == generation && !background.contains(id))
            });
        if !background && turn_stopped {
            continue;
        }
        if let Some(stop) = latest_stop
            && !background
            && (fact.at_ms <= stop.at_ms || resume <= stop.at_ms)
        {
            continue;
        }
        if !background && idle_at_ms.is_some_and(|idle| fact.at_ms <= idle) {
            let _ = record(
                state,
                parent,
                Event::Stop {
                    id: id.clone(),
                    generation: generation.clone(),
                },
                now_ms,
            );
            continue;
        }
        runs.push(Running {
            id: id.clone(),
            kind: kind
                .as_deref()
                .and_then(SubagentText::parse)
                .map(|text| text.0),
            description: description
                .as_deref()
                .and_then(SubagentText::parse)
                .map(|text| text.0),
            started_at_ms: fact.at_ms,
        });
    }
    runs.sort_by(|a, b| (a.started_at_ms, &a.id).cmp(&(b.started_at_ms, &b.id)));
    let mut seen = std::collections::HashSet::new();
    runs.reverse();
    runs.retain(|run| seen.insert(run.id.clone()));
    runs.reverse();
    runs
}

/// A digest over names and file metadata catches file-only changes without
/// requiring a database write or filesystem watcher.
pub fn revision(state: &Path) -> [u8; 32] {
    let mut entries = Vec::new();
    if let Ok(directories) = std::fs::read_dir(state.join("subagents/v1")) {
        for (count, directory) in directories.enumerate().take(4097) {
            if count == 4096 {
                return [0xff; 32];
            }
            let Ok(directory) = directory else { continue };
            if !directory.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(directory.path()) {
                for entry in files.take(FACTS_PER_PARENT + 1).filter_map(Result::ok) {
                    if entries.len() == 32768 {
                        return [0xff; 32];
                    }
                    if let Ok(meta) = entry.metadata() {
                        entries.push((entry.path(), meta.len(), meta.modified().ok()));
                    }
                }
            }
        }
    }
    entries.sort();
    Sha256::digest(format!("{entries:?}").as_bytes()).into()
}
