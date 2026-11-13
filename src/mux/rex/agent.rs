//! Who runs in a block: the liveness rule the survey and the gate both
//! call, so they cannot disagree, and the block's shell.

use super::{
    binding::Binding,
    wire::{Proc, ProcessReply, StatusRecord},
};
use crate::{
    AgentRuntime, AgentSessionId,
    mux::{
        AgentIdentity, AgentKind, AgentState, SessionClaim,
        processes::{LocalProcesses, LocalRead, is_shell},
    },
};
use std::path::Path;

/// Decided from the block's `host_id` before any local read, so a remote
/// block's pids are never read here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Locality {
    NotLocal,
    Local {
        /// The foreground pid's read. `Gone` when Rex reports no foreground.
        foreground: LocalRead,
        /// The bound agent's read, when the binding's agent is not the
        /// foreground. When it is, the foreground's read decides.
        bound_agent: Option<LocalRead>,
    },
}

impl Locality {
    /// A block whose host differs from its server's, or where either is
    /// unknown, runs on another machine, as Rex's docs say to assume.
    pub(super) fn read(
        local: &dyn LocalProcesses,
        block_host: Option<&str>,
        server_host: Option<&str>,
        foreground: Option<&Proc>,
        binding: Option<&Binding>,
    ) -> Self {
        match (block_host, server_host) {
            (Some(block), Some(server)) if block == server => Self::Local {
                foreground: foreground.map_or(LocalRead::Gone, |process| local.read(process.pid)),
                bound_agent: binding
                    .and_then(Binding::launched_agent)
                    .map(|pid| local.read(pid)),
            },
            _ => Self::NotLocal,
        }
    }
}

pub(super) struct Observed<'a> {
    /// The root record.
    pub record: Option<&'a StatusRecord>,
    /// `process.foreground`.
    pub foreground: Option<&'a Proc>,
    pub binding: Option<&'a Binding>,
    pub locality: Locality,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PaneAgent {
    pub agent: Option<AgentIdentity>,
    pub state: Option<AgentState>,
    pub reports_session: bool,
}

impl PaneAgent {
    const FREE: Self = Self {
        agent: None,
        state: None,
        reports_session: true,
    };

    fn known(kind: AgentRuntime, session: SessionClaim, state: &str) -> Self {
        Self {
            reports_session: matches!(session, SessionClaim::Reported(_)),
            agent: Some(AgentIdentity {
                kind: AgentKind::Known(kind),
                label: Some(kind.as_str().to_owned()),
                session,
            }),
            state: Some(AgentState::parse(state)),
        }
    }
}

/// A record speaks for the block only while its owner is the live foreground
/// process, pid and name both, and its `app` is an agent Namedrop knows. A
/// session is reported only while the binding names the foreground's
/// incarnation, the bound agent lives with its bound start in the
/// foreground's group, and the binding's runtime is the record's `app`. So a
/// replaced program loses its session, and a launcher whose agent died stops
/// speaking for it. ADAPTER.md's liveness table has the rows.
pub(super) fn pane_agent(observed: Observed<'_>) -> PaneAgent {
    let Observed {
        record,
        foreground,
        binding,
        locality,
    } = observed;
    let Some(record) = record else {
        return unreported_kind(binding, &locality);
    };
    let speaks = foreground.is_some_and(|foreground| {
        record
            .owner
            .as_ref()
            .is_some_and(|owner| owner.pid == foreground.pid && owner.name == foreground.name)
    });
    let kind = record
        .app
        .as_deref()
        .and_then(|app| AgentRuntime::parse(app).ok());
    let (true, Some(kind)) = (speaks, kind) else {
        return PaneAgent::FREE;
    };
    let (foreground, bound_agent) = match locality {
        Locality::NotLocal
        | Locality::Local {
            foreground: LocalRead::Unreadable,
            ..
        } => return PaneAgent::known(kind, SessionClaim::None, &record.state),
        Locality::Local {
            foreground: LocalRead::Gone,
            ..
        } => return PaneAgent::FREE,
        Locality::Local {
            foreground: LocalRead::Alive(foreground),
            bound_agent,
        } => (foreground, bound_agent),
    };
    let Some(binding) = binding.filter(|binding| binding.foreground == foreground.incarnation.0)
    else {
        let incarnation = foreground.incarnation;
        return PaneAgent::known(
            kind,
            SessionClaim::Unreported { incarnation },
            &record.state,
        );
    };
    if lives(binding, foreground.group, bound_agent.as_ref()) && binding.runtime == kind.as_str() {
        let session = SessionClaim::Reported(AgentSessionId::new(&binding.session));
        PaneAgent::known(kind, session, &record.state)
    } else {
        PaneAgent::FREE
    }
}

/// No record yet, as when a hook beats its agent's first report: a binding
/// to the live foreground still names the session, with no kind or state.
fn unreported_kind(binding: Option<&Binding>, locality: &Locality) -> PaneAgent {
    let Locality::Local {
        foreground: LocalRead::Alive(foreground),
        bound_agent,
    } = locality
    else {
        return PaneAgent::FREE;
    };
    match binding {
        Some(binding)
            if binding.foreground == foreground.incarnation.0
                && lives(binding, foreground.group, bound_agent.as_ref()) =>
        {
            PaneAgent {
                agent: Some(AgentIdentity {
                    kind: AgentKind::Unreported,
                    label: None,
                    session: SessionClaim::Reported(AgentSessionId::new(&binding.session)),
                }),
                state: None,
                reports_session: true,
            }
        }
        _ => PaneAgent::FREE,
    }
}

/// The bound agent is the foreground itself, or it is alive with its bound
/// start time in the foreground's group.
fn lives(binding: &Binding, group: u32, bound_agent: Option<&LocalRead>) -> bool {
    if binding.agent == binding.foreground {
        return true;
    }
    matches!(bound_agent, Some(LocalRead::Alive(agent))
        if agent.incarnation.0 == binding.agent && agent.group == group)
}

/// The block's shell, or `None`. The candidate is the child, or, when the
/// child is `login` (macOS), the process just below it in the foreground and
/// its ancestors. It counts only when its executable is a shell, so a block
/// made with `-- python3 -i`, an editor, or `ssh` has none. When the
/// candidate is the foreground itself, it counts only when `foreground_pids`,
/// the live members of its group, holds no other process: a non-interactive
/// shell runs its commands in its own group. `None` also when Rex leaves the
/// ancestors out, for that read only.
pub(super) fn shell_of(process: &ProcessReply, foreground_pids: &[u32]) -> Option<u32> {
    let child = process.child.as_ref()?;
    let foreground = process.foreground.as_ref()?;
    let candidate = if child.name == "login" {
        let ancestors = process.ancestors.as_deref().unwrap_or_default();
        let chain: Vec<&Proc> = std::iter::once(foreground).chain(ancestors).collect();
        let login = chain.iter().position(|process| process.pid == child.pid)?;
        *chain.get(login.checked_sub(1)?)?
    } else {
        if foreground.pid != child.pid && process.ancestors.is_none() {
            return None;
        }
        child
    };
    if !candidate
        .path
        .as_deref()
        .is_some_and(|path| is_shell(Path::new(path)))
    {
        return None;
    }
    if candidate.pid == foreground.pid && foreground_pids.iter().any(|pid| *pid != candidate.pid) {
        return None;
    }
    Some(candidate.pid)
}
