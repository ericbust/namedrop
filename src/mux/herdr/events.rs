//! Herdr plugin events, decoded to `MuxEvent`. The structs mirror Herdr's
//! JSON with `deny_unknown_fields`, so a changed payload fails loudly.

use super::wire::AgentSessionEntry;
use crate::{
    SpaceId,
    mux::{MuxEvent, PaneId, PaneSlot, TabId},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginEventEnvelope {
    event: String,
    data: Value,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum PaneLifecycleEvent {
    #[serde(rename = "pane_created")]
    Created { pane: PaneEventInfo },
    #[serde(rename = "pane_closed")]
    Closed {
        pane_id: String,
        workspace_id: String,
    },
    #[serde(rename = "pane_exited")]
    Exited {
        pane_id: String,
        workspace_id: String,
    },
    #[serde(rename = "tab_closed")]
    TabClosed {
        tab_id: String,
        workspace_id: String,
    },
    #[serde(rename = "pane_moved")]
    Moved {
        previous_pane_id: String,
        previous_workspace_id: String,
        previous_tab_id: String,
        pane: PaneEventInfo,
        #[serde(rename = "created_tab")]
        _created_tab: Option<Value>,
        #[serde(rename = "created_workspace")]
        _created_workspace: Option<Value>,
        #[serde(rename = "closed_tab_id")]
        _closed_tab_id: Option<String>,
        #[serde(rename = "closed_workspace_id")]
        _closed_workspace_id: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStatusChangedEvent {
    #[serde(rename = "type")]
    event_type: AgentStatusEventType,
    pane_id: String,
    workspace_id: String,
    #[serde(rename = "agent_status")]
    _agent_status: Option<String>,
    #[serde(rename = "released")]
    _released: Option<bool>,
    #[serde(rename = "final_status")]
    _final_status: Option<String>,
    #[serde(rename = "agent")]
    _agent: Option<String>,
    #[serde(rename = "display_agent")]
    _display_agent: Option<String>,
    #[serde(rename = "title")]
    _title: Option<String>,
    #[serde(default, rename = "state_labels")]
    _state_labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
enum AgentStatusEventType {
    #[serde(rename = "pane_agent_status_changed")]
    Changed,
    #[serde(rename = "pane_agent_detected")]
    Detected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PaneEventInfo {
    pane_id: String,
    workspace_id: String,
    terminal_id: String,
    tab_id: String,
    focused: bool,
    agent_status: String,
    revision: u64,
    agent: Option<String>,
    agent_session: Option<AgentSessionEntry>,
    cwd: Option<String>,
    display_agent: Option<String>,
    foreground_cwd: Option<String>,
    label: Option<String>,
    scroll: Option<Value>,
    #[serde(default)]
    state_labels: BTreeMap<String, String>,
    terminal_title: Option<String>,
    terminal_title_stripped: Option<String>,
    title: Option<String>,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

impl PaneEventInfo {
    fn slot(&self) -> PaneSlot {
        slot(&self.workspace_id, &self.pane_id, Some(&self.tab_id))
    }
}

fn slot(space: &str, pane: &str, tab: Option<&str>) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: tab.map(TabId::new),
    }
}

/// A plugin hook's `HERDR_PLUGIN_EVENT_JSON`, checked against the hook event
/// and the operation the manifest routes it to.
pub fn decode_plugin_event(
    operation: &str,
    hook_event: &str,
    serialized: &str,
) -> Result<MuxEvent, String> {
    let envelope: PluginEventEnvelope =
        serde_json::from_str(serialized).map_err(|error| error.to_string())?;
    let expected = match envelope.event.as_str() {
        "pane_created" => ("pane.created", "event.pane-lifecycle"),
        "pane_moved" => ("pane.moved", "event.pane-lifecycle"),
        "pane_closed" => ("pane.closed", "event.pane-lifecycle"),
        "pane_exited" => ("pane.exited", "event.pane-lifecycle"),
        "tab_closed" => ("tab.closed", "event.pane-lifecycle"),
        "pane_agent_status_changed" => ("pane.agent_status_changed", "event.agent-status-changed"),
        "pane_agent_detected" => ("pane.agent_detected", "event.agent-status-changed"),
        other => return Err(format!("unsupported Herdr plugin event: {other}")),
    };
    if hook_event != expected.0 || operation != expected.1 {
        return Err(format!(
            "Herdr plugin event {} does not match operation {operation}",
            envelope.event
        ));
    }
    decode_operation_input(operation, envelope.data)
}

/// The Herdr event data an `event.*` operation receives as its input.
pub fn decode_operation_input(operation: &str, input: Value) -> Result<MuxEvent, String> {
    match operation {
        "event.pane-lifecycle" => Ok(
            match serde_json::from_value(input).map_err(crate::input_schema_error)? {
                PaneLifecycleEvent::Created { pane } => MuxEvent::PaneAppeared(pane.slot()),
                PaneLifecycleEvent::Closed {
                    pane_id,
                    workspace_id,
                }
                | PaneLifecycleEvent::Exited {
                    pane_id,
                    workspace_id,
                } => MuxEvent::PaneClosed(slot(&workspace_id, &pane_id, None)),
                PaneLifecycleEvent::TabClosed {
                    tab_id,
                    workspace_id,
                } => MuxEvent::TabClosed {
                    space: SpaceId::new(workspace_id),
                    tab: TabId::new(tab_id),
                },
                PaneLifecycleEvent::Moved {
                    previous_pane_id,
                    previous_workspace_id,
                    previous_tab_id,
                    pane,
                    ..
                } => MuxEvent::PaneMoved {
                    from: slot(
                        &previous_workspace_id,
                        &previous_pane_id,
                        Some(&previous_tab_id),
                    ),
                    to: pane.slot(),
                },
            },
        ),
        "event.agent-status-changed" => {
            let event: AgentStatusChangedEvent =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            let pane = slot(&event.workspace_id, &event.pane_id, None);
            Ok(match event.event_type {
                AgentStatusEventType::Changed => MuxEvent::AgentStatusChanged(pane),
                AgentStatusEventType::Detected => MuxEvent::AgentDetected(pane),
            })
        }
        other => Err(format!("{other} is not a Herdr event operation")),
    }
}
