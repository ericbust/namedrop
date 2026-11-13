//! Session bindings: which agent session runs in a Rex block, as the
//! block's own Namedrop hook recorded it. Rex reports no session, so this file
//! is the only witness. Only `Place::bind_session` writes one, and a refused
//! bind leaves its reason for Doctor, because the hook itself stays silent.

use super::wire::{Proc, StatusRecord};
use crate::{
    AgentRuntime, AgentSessionId, SpaceId,
    mux::{
        Endpoint, PaneId,
        home::{BindRefused, Deferral, Place},
        processes::{LocalProc, LocalProcesses, LocalRead, ThisMachine, is_shell},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    /// `block:<uuid>`.
    pub block: String,
    /// The canonical endpoint, for a sandboxed deferral.
    pub endpoint: String,
    /// `session:<uuid>`, for a sandboxed deferral.
    pub space: String,
    /// The block's foreground when the hook ran, as `<pid>@<start>`.
    pub foreground: String,
    /// The hook's agent, as `<pid>@<start>`. Equal to `foreground` when the
    /// agent is the foreground itself, and otherwise behind a launcher.
    pub agent: String,
    /// The wrapper's runtime, which the record's `app` must equal.
    pub runtime: String,
    /// The hook's `session_id`.
    pub session: String,
    pub bound_at_ms: u64,
}

impl Binding {
    /// The pid of the bound agent, when it is not the foreground and so
    /// needs a read of its own.
    pub(super) fn launched_agent(&self) -> Option<u32> {
        if self.agent == self.foreground {
            return None;
        }
        self.agent.split_once('@')?.0.parse().ok()
    }

    /// The same binding, whenever it was written.
    fn same(&self, other: &Self) -> bool {
        Self {
            bound_at_ms: other.bound_at_ms,
            ..self.clone()
        } == *other
    }
}

/// The latest refused bind of a block, which Doctor lists while the block
/// has no session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Refusal {
    pub block: String,
    pub endpoint: String,
    pub condition: BindRefused,
    pub session: String,
    pub at_ms: u64,
}

/// The bindings under `<state>/rex/sessions/` and the refusals under
/// `<state>/rex/refusals/`, one file per block each.
#[derive(Clone, Debug)]
pub(super) struct Bindings {
    sessions: PathBuf,
    refusals: PathBuf,
}

impl Bindings {
    pub(super) fn new(state: &Path) -> Self {
        let rex = state.join("rex");
        Self {
            sessions: rex.join("sessions"),
            refusals: rex.join("refusals"),
        }
    }

    /// `<uuid>.json` for `block:<uuid>`. Block IDs come from the server, so
    /// one that is not a UUID names no file.
    pub(super) fn path(&self, block: &str) -> Option<PathBuf> {
        file_name(block).map(|name| self.sessions.join(name))
    }

    fn refusal_path(&self, block: &str) -> Option<PathBuf> {
        file_name(block).map(|name| self.refusals.join(name))
    }

    /// The block's binding. A missing, unreadable, or malformed file, or one
    /// that names another block, is no binding.
    pub(super) fn read(&self, block: &str) -> Option<Binding> {
        let text = std::fs::read(self.path(block)?).ok()?;
        serde_json::from_slice::<Binding>(&text)
            .ok()
            .filter(|binding| binding.block == block)
    }

    /// The block's latest refused bind, read as `read` reads a binding.
    pub(super) fn refusal(&self, block: &str) -> Option<Refusal> {
        let text = std::fs::read(self.refusal_path(block)?).ok()?;
        serde_json::from_slice::<Refusal>(&text)
            .ok()
            .filter(|refusal| refusal.block == block)
    }

    /// Writes `binding` unless the same one is already there, and deletes the
    /// block's refusal either way.
    fn write(&self, binding: &Binding) -> io::Result<()> {
        let unnamed = || io::Error::other(format!("{} names no file", binding.block));
        if !self
            .read(&binding.block)
            .is_some_and(|bound| bound.same(binding))
        {
            replace(&self.path(&binding.block).ok_or_else(unnamed)?, binding)?;
        }
        match std::fs::remove_file(self.refusal_path(&binding.block).ok_or_else(unnamed)?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn refuse(&self, refusal: &Refusal) -> io::Result<()> {
        match self.refusal_path(&refusal.block) {
            Some(path) => replace(&path, refusal),
            None => Ok(()),
        }
    }
}

fn file_name(block: &str) -> Option<String> {
    let uuid = block.strip_prefix("block:")?;
    (!uuid.is_empty() && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then(|| format!("{uuid}.json"))
}

/// A temp file beside `path`, then a rename, so a reader sees the old file
/// or the new one.
fn replace(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let directory = path.parent().expect("a binding file has a directory");
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("binding"),
        std::process::id()
    ));
    std::fs::write(
        &temporary,
        serde_json::to_vec(value).map_err(io::Error::other)?,
    )?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// What the verifying read showed about the caller's block, and this
/// process's ancestors when it was read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Caller {
    /// `process.foreground`.
    pub(super) foreground: Option<Proc>,
    /// The root status record.
    pub(super) record: Option<StatusRecord>,
    /// Parent first.
    pub(super) ancestors: Vec<u32>,
}

impl Caller {
    pub(crate) fn of(self, ancestors: Vec<u32>) -> Self {
        Self { ancestors, ..self }
    }
}

/// The process whose hook this is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HookAgent {
    /// Where it stands in the caller's ancestors.
    pub at: usize,
    pub pid: u32,
    pub executable: PathBuf,
}

/// The nearest of `ancestors` whose executable is not a shell by
/// `is_shell`, never by `comm`, which Linux sets to a script's truncated
/// name. `None` when an executable on the way cannot be read, because a
/// shell then cannot be told from an agent.
pub(super) fn hook_agent(local: &dyn LocalProcesses, ancestors: &[u32]) -> Option<HookAgent> {
    for (at, pid) in ancestors.iter().copied().enumerate() {
        let executable = local.executable(pid)?;
        if !is_shell(&executable) {
            return Some(HookAgent {
                at,
                pid,
                executable,
            });
        }
    }
    None
}

impl Place {
    /// Records that `session` runs in the caller's block, reading this
    /// machine's processes. `run_context_hook` calls it for SessionStart and
    /// UserPromptSubmit, and ignores the answer, which the refusal file keeps.
    pub fn bind_session(
        &self,
        state: &Path,
        session: &AgentSessionId,
        runtime: AgentRuntime,
        entrypoint: Option<&OsStr>,
    ) -> Result<(), BindRefused> {
        self.bind(state, session, runtime, entrypoint, &ThisMachine, now_ms())
    }

    /// Let F be the block's foreground and H the hook's agent. Refuses unless
    /// all of these hold:
    /// 1. H is in F's process group.
    /// 2. H is F, or F is an ancestor of H, every process between them is a
    ///    shell, and F's executable differs from H's (the launcher rule).
    /// 3. A root record whose owner is F (pid and name) has `app` equal to
    ///    `runtime`. A record F does not own is stale and counts as absent.
    /// 4. The reads of F and H are both `Alive`. Conditions 1 and 2 need
    ///    those reads, so this one is checked first.
    /// 5. When H is not F, the block's binding does not already name F's
    ///    incarnation as its agent. When H is F, the bind replaces any binding.
    /// 6. When H is not F and `runtime` is Claude Code, `entrypoint` is not
    ///    `sdk-cli`, which marks a nested print-mode agent.
    ///
    /// Writes only a change. A refusal overwrites the block's refusal file,
    /// and a bind deletes it. A place with no verified caller has no block
    /// to record a refusal for.
    pub(crate) fn bind(
        &self,
        state: &Path,
        session: &AgentSessionId,
        runtime: AgentRuntime,
        entrypoint: Option<&OsStr>,
        local: &dyn LocalProcesses,
        now_ms: u64,
    ) -> Result<(), BindRefused> {
        let (Some(caller), Some(space), Some(block)) = (&self.caller, &self.space, &self.pane)
        else {
            return Err(BindRefused::NoCaller);
        };
        let bindings = Bindings::new(state);
        let block = block.as_str();
        let decided = decide(
            caller,
            runtime,
            entrypoint,
            local,
            bindings.read(block).as_ref(),
        )
        .and_then(|(foreground, agent)| {
            let binding = Binding {
                block: block.to_owned(),
                endpoint: self.endpoint.key.clone(),
                space: space.as_str().to_owned(),
                foreground: foreground.incarnation.0,
                agent: agent.incarnation.0,
                runtime: runtime.as_str().to_owned(),
                session: session.as_str().to_owned(),
                bound_at_ms: now_ms,
            };
            bindings
                .write(&binding)
                .map_err(|error| BindRefused::Io(error.to_string()))
        });
        if let Err(condition) = &decided {
            let _ = bindings.refuse(&Refusal {
                block: block.to_owned(),
                endpoint: self.endpoint.key.clone(),
                condition: condition.clone(),
                session: session.as_str().to_owned(),
                at_ms: now_ms,
            });
        }
        decided
    }
}

/// The foreground's and the hook agent's live reads, when the six
/// conditions hold.
fn decide(
    caller: &Caller,
    runtime: AgentRuntime,
    entrypoint: Option<&OsStr>,
    local: &dyn LocalProcesses,
    bound: Option<&Binding>,
) -> Result<(LocalProc, LocalProc), BindRefused> {
    let foreground = caller.foreground.as_ref().ok_or(BindRefused::NotAlive)?;
    let agent = hook_agent(local, &caller.ancestors).ok_or(BindRefused::NotAlive)?;
    let (LocalRead::Alive(f), LocalRead::Alive(h)) =
        (local.read(foreground.pid), local.read(agent.pid))
    else {
        return Err(BindRefused::NotAlive);
    };
    if h.group != f.group {
        return Err(BindRefused::NotInGroup);
    }
    let behind = agent.pid != foreground.pid;
    if behind && !launches(local, &caller.ancestors, &agent, foreground.pid) {
        return Err(BindRefused::Launcher);
    }
    if let Some(record) = &caller.record
        && record
            .owner
            .as_ref()
            .is_some_and(|owner| owner.pid == foreground.pid && owner.name == foreground.name)
        && record.app.as_deref() != Some(runtime.as_str())
    {
        return Err(BindRefused::Runtime);
    }
    if behind && bound.is_some_and(|binding| binding.agent == f.incarnation.0) {
        return Err(BindRefused::AlreadyBound);
    }
    if behind
        && runtime == AgentRuntime::ClaudeCode
        && entrypoint.is_some_and(|entrypoint| entrypoint == "sdk-cli")
    {
        return Err(BindRefused::NestedPrintMode);
    }
    Ok((f, h))
}

/// The foreground is an ancestor of the hook's agent with only shells
/// between them, and runs another executable: a launcher, not an agent that
/// started the hook's agent.
fn launches(
    local: &dyn LocalProcesses,
    ancestors: &[u32],
    agent: &HookAgent,
    foreground: u32,
) -> bool {
    let Some(at) = ancestors
        .iter()
        .skip(agent.at + 1)
        .position(|pid| *pid == foreground)
        .map(|offset| agent.at + 1 + offset)
    else {
        return false;
    };
    ancestors[agent.at + 1..at]
        .iter()
        .all(|pid| local.executable(*pid).is_some_and(|path| is_shell(&path)))
        && local
            .executable(foreground)
            .is_some_and(|path| path != agent.executable)
}

/// The place `block`'s binding under `state` names, for a sandboxed
/// command's deferral, with the bound agent's incarnation that must prove
/// the caller.
pub(crate) fn bound(state: &Path, block: &str) -> Option<(Deferral, String)> {
    let binding = Bindings::new(state).read(block)?;
    Some((
        Deferral {
            endpoint: Endpoint::parse(&binding.endpoint),
            space: SpaceId::new(binding.space),
            pane: PaneId::new(binding.block),
        },
        binding.agent,
    ))
}

/// The latest refused bind of `block` under `state`, for Doctor.
pub(crate) fn refusal(state: &Path, block: &str) -> Option<Refusal> {
    Bindings::new(state).refusal(block)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests;
