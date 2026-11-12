//! The world as Herdr's read commands print it, for tests that hand Herdr's
//! replies to a script `herdr` and for the conformance suite's transport.

use super::{FakeMux, FakePane, FakeSpace, World, agent_word};
use crate::mux::{AgentState, PaneId, SessionClaim, TabId};
use serde_json::{Value, json};
use std::path::Path;

fn herdr_error(code: &str) -> String {
    json!({"error": {"code": code}}).to_string()
}

fn space<'w>(world: &'w World, id: &str) -> Result<&'w FakeSpace, String> {
    world
        .spaces
        .iter()
        .find(|space| space.id.as_str() == id)
        .ok_or_else(|| herdr_error("workspace_not_found"))
}

pub(crate) fn pane<'w>(world: &'w World, id: &str) -> Result<&'w FakePane, String> {
    world
        .pane(&PaneId::new(id))
        .ok_or_else(|| herdr_error("pane_not_found"))
}

fn pane_json(pane: &FakePane) -> Value {
    let facts = &pane.facts;
    let agent = facts.agent.as_ref();
    let word = agent.and_then(|agent| agent_word(&agent.kind));
    let display = agent
        .and_then(|agent| agent.label.clone())
        .filter(|label| Some(label) != word.as_ref());
    let session = agent.and_then(|agent| match &agent.session {
        SessionClaim::Reported(session) => Some(json!({"value": session.as_str()})),
        _ => None,
    });
    let mut json = json!({
        "pane_id": facts.id.as_str(),
        "terminal_id": facts.terminal.as_str(),
        "tab_id": facts.tab.as_str(),
        "workspace_id": facts.space.as_str(),
        "cwd": facts.cwd,
        "label": facts.title,
        "agent": word,
        "display_agent": display,
        "agent_session": session,
        "agent_status": facts.state.as_ref().map(AgentState::as_str),
        "state_labels": facts.idle_label.as_ref().map(|idle| json!({"idle": idle})),
        "scroll": facts.viewport_rows.map(|rows| json!({"viewport_rows": rows})),
    });
    if !facts.reports_session {
        json.as_object_mut()
            .expect("a pane is an object")
            .remove("agent_session");
    }
    json
}

fn stamp(space: &FakeSpace) -> Option<Value> {
    space.stamped().map(|checkout| {
        json!({
            "checkout_path": checkout.path,
            "repo_key": checkout.repo_key,
            "repo_name": checkout.repo_name,
        })
    })
}

/// Herdr's reply to a read command, or `None` for a command that is not one.
pub(crate) fn reply(world: &World, args: &[&str]) -> Option<Result<Value, String>> {
    let answered = |value: Value| Some(Ok(value));
    match args {
        ["worktree", "list", "--workspace", id, "--json"] => {
            Some(space(world, id).and_then(|space| {
                let checkout = space
                    .checkout
                    .as_ref()
                    .ok_or_else(|| herdr_error("not_git_worktree"))?;
                Ok(json!({"result": {
                    "source": {"repo_key": checkout.repo_key},
                    "worktrees": [{
                        "path": checkout.path, "open_workspace_id": id,
                        "label": checkout.label, "branch": checkout.branch,
                        "is_linked_worktree": true,
                    }],
                }}))
            }))
        }
        ["worktree", "list", "--cwd", cwd, "--json"] => {
            let opener = world.spaces.iter().find_map(|space| {
                let checkout = space.checkout.as_ref()?;
                (checkout.path == Path::new(cwd)).then_some((space, checkout))
            });
            answered(match opener {
                Some((space, checkout)) => json!({"result": {
                    "source": {"repo_key": checkout.repo_key},
                    "worktrees": [{
                        "path": cwd, "open_workspace_id": space.id.as_str(),
                        "label": checkout.label, "branch": checkout.branch,
                        "is_linked_worktree": true,
                    }],
                }}),
                None => json!({"result": {"source": null, "worktrees": []}}),
            })
        }
        ["workspace", "list"] => {
            let workspaces: Vec<_> = world
                .spaces
                .iter()
                .filter(|space| space.listed)
                .map(|space| {
                    json!({
                        "workspace_id": space.id.as_str(),
                        "label": space.label,
                        "number": space.number,
                        "worktree": stamp(space),
                    })
                })
                .collect();
            answered(json!({"result": {"workspaces": workspaces}}))
        }
        ["workspace", "get", id] => Some(space(world, id).map(|space| {
            json!({"result": {"workspace": {
                "label": space.label,
                "number": space.number,
                "worktree": stamp(space),
            }}})
        })),
        ["pane", "list"] => {
            let panes: Vec<_> = world
                .spaces
                .iter()
                .flat_map(FakeSpace::listed_panes)
                .map(pane_json)
                .collect();
            answered(json!({"result": {"panes": panes}}))
        }
        ["pane", "list", "--workspace", id] => Some(space(world, id).map(|space| {
            let panes: Vec<_> = space.listed_panes().map(pane_json).collect();
            json!({"result": {"panes": panes}})
        })),
        ["pane", "read", id, "--source", _, "--lines", lines, ..] => {
            Some(pane(world, id).map(|pane| {
                let screen = pane.screen.clone().unwrap_or_default();
                let lines: usize = lines.parse().expect("a line count");
                let all: Vec<_> = screen.lines().collect();
                Value::String(all[all.len().saturating_sub(lines)..].join("\n"))
            }))
        }
        ["pane", "process-info", "--pane", id] => Some(pane(world, id).map(|pane| {
            let process = pane.process.clone().unwrap_or_default();
            let foreground: Vec<_> = process
                .foreground_pids
                .iter()
                .map(|pid| json!({"pid": pid}))
                .collect();
            json!({"result": {"process_info": {
                "shell_pid": process.shell_pid,
                "foreground_process_group_id": process.foreground_group,
                "foreground_processes": foreground,
                "tty": process.tty,
            }}})
        })),
        ["tab", "get", tab] => answered(match world.tab_labels.get(&TabId::new(*tab)) {
            Some(label) => json!({"result": {"label": label}}),
            None => json!({"result": {}}),
        }),
        ["plugin", "list", "--plugin", "namedrop", "--json"] => {
            let plugins: Vec<_> = world
                .integration_root
                .iter()
                .map(|root| json!({"plugin_id": "namedrop", "plugin_root": root}))
                .collect();
            answered(json!({"result": {"plugins": plugins}}))
        }
        _ => None,
    }
}

/// Herdr's stdout for a reply: `pane read` prints its text, the rest JSON.
pub(crate) fn stdout(value: Value) -> String {
    match value {
        Value::String(text) => text,
        json => json.to_string(),
    }
}

impl FakeMux {
    /// What Herdr prints for the read command `args` in this world: its
    /// stdout, or, for a missing space or pane, its error JSON. It reads the
    /// world as it is, without logging a read or running a hook, so a test
    /// that applies switches in a read hook applies them first. A test that
    /// hands a script `herdr` its replies builds them here, so the script
    /// serves the world the in-process code reads.
    ///
    /// # Panics
    ///
    /// For a command that is not one of Herdr's reads: `workspace list` or
    /// `get`, `worktree list`, `pane list`, `pane read`, `pane process-info`,
    /// `tab get`, or `plugin list`.
    pub fn herdr_reply(&self, args: &[&str]) -> Result<String, String> {
        reply(&self.lock(), args)
            .unwrap_or_else(|| panic!("herdr_reply renders only Herdr's reads, not {args:?}"))
            .map(stdout)
    }
}
