//! The Rex module's stable action names and its per-user registration.

use crate::{installed_runtime::PackageLease, mux::Endpoint};
use serde_json::{Value, json};
use std::{fs, path::Path};

mod handoff;
mod install;
mod sessions;
pub(crate) use handoff::{handoff, predecessor_warnings};
pub use install::{confirm_remove, install, remove};
pub use sessions::sessions;

pub const CONFIG_LINE: &str = "pcall(require, \"namedrop\");";
pub const MANAGED_LINE: &str =
    "-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Activate,
    Popup(crate::cli::UiMode),
    Channels,
    Pause,
    Resume,
    Deactivate,
}

pub struct ActionDefinition {
    pub name: &'static str,
    pub title: &'static str,
    pub key: Option<&'static str>,
    pub action: Action,
}

pub const ACTIONS: &[ActionDefinition] = &[
    ActionDefinition {
        name: "activate",
        title: "Activate Namedrop",
        key: None,
        action: Action::Activate,
    },
    ActionDefinition {
        name: "name",
        title: "Name this pane",
        key: Some("alt+shift+cmd+i"),
        action: Action::Popup(crate::cli::UiMode::Name),
    },
    ActionDefinition {
        name: "manage",
        title: "Manage Namedrop names and agents",
        key: Some("alt+shift+cmd+m"),
        action: Action::Popup(crate::cli::UiMode::Manage),
    },
    ActionDefinition {
        name: "doctor",
        title: "Namedrop Doctor",
        key: Some("alt+shift+cmd+h"),
        action: Action::Popup(crate::cli::UiMode::Doctor),
    },
    ActionDefinition {
        name: "status",
        title: "Agent status",
        key: Some("alt+shift+cmd+s"),
        action: Action::Popup(crate::cli::UiMode::Status),
    },
    ActionDefinition {
        name: "tour",
        title: "Namedrop tour",
        key: Some("alt+shift+cmd+o"),
        action: Action::Popup(crate::cli::UiMode::Tour),
    },
    ActionDefinition {
        name: "channels",
        title: "Open Namedrop Channels",
        key: None,
        action: Action::Channels,
    },
    ActionDefinition {
        name: "pause",
        title: "Pause Namedrop",
        key: None,
        action: Action::Pause,
    },
    ActionDefinition {
        name: "resume",
        title: "Resume Namedrop",
        key: None,
        action: Action::Resume,
    },
    ActionDefinition {
        name: "deactivate",
        title: "Deactivate Namedrop",
        key: None,
        action: Action::Deactivate,
    },
];

pub fn action(name: &str) -> Result<Action, String> {
    ACTIONS
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.action)
        .ok_or_else(|| {
            format!("This Namedrop has no action {name}. Run sh scripts/install.sh again.")
        })
}

/// A Lua 5.1 quoted byte string. JSON's Unicode escapes are not Lua escapes.
fn lua_string(text: &str) -> String {
    let mut out = String::from("\"");
    for byte in text.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            32..=126 => out.push(char::from(byte)),
            _ => out.push_str(&format!("\\{byte:03}")),
        }
    }
    out.push('"');
    out
}

pub fn module_text(launcher: &Path, state: &Path) -> Result<String, String> {
    fn path(p: &Path) -> Result<&str, String> {
        p.to_str()
            .filter(|_| p.is_absolute())
            .ok_or_else(|| format!("Namedrop needs an absolute UTF-8 path: {}", p.display()))
    }
    let mut actions = String::from("local actions = {\n");
    for a in ACTIONS {
        actions.push_str(&format!(
            "\t{{ {}, {}{} }},\n",
            lua_string(a.name),
            lua_string(a.title),
            a.key
                .map(|k| format!(", {}", lua_string(k)))
                .unwrap_or_default()
        ));
    }
    actions.push('}');
    let launcher = lua_string(path(launcher)?);
    let state = lua_string(path(state)?);
    let mut module = String::new();
    for line in include_str!("rex_ui/module.lua").lines() {
        match line {
            "local NAMEDROP = __LAUNCHER__" => {
                module.push_str(&format!("local NAMEDROP = {launcher}"))
            }
            "local STATE = __STATE__" => module.push_str(&format!("local STATE = {state}")),
            "__ACTIONS__" => module.push_str(&actions),
            _ => module.push_str(line),
        }
        module.push('\n');
    }
    Ok(module)
}

pub(crate) fn registration(home: &Path) -> Result<Option<Value>, String> {
    let path = crate::locator::rex_registration(home);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))
            .and_then(|v| {
                if v.is_object() {
                    Ok(Some(v))
                } else {
                    Err(format!("{} is not an object", path.display()))
                }
            }),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

pub(crate) fn refresh(
    lease: &PackageLease,
    home: &Path,
    launcher: &Path,
    state: &Path,
) -> Result<(), String> {
    let Some(mut record) = registration(home)? else {
        return Ok(());
    };
    let Some(module) = record["module"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
    else {
        return Ok(());
    };
    lease.require_write()?;
    if !module.is_absolute() {
        return Err("The registered namedrop.lua path is not absolute".into());
    }
    match fs::read_to_string(&module) {
        Ok(old) if old.lines().next() != Some(MANAGED_LINE) => {
            return Err(format!("{} is not managed by Namedrop", module.display()));
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    let text = module_text(launcher, state)?;
    crate::install::replace_if_changed(lease, &module, &text, Some(0o600))?;
    reinspect_line(&mut record);
    record["launcher"] = json!(launcher);
    record["state"] = json!(state);
    crate::install::replace_if_changed(
        lease,
        &crate::locator::rex_registration(home),
        &(serde_json::to_string_pretty(&record).map_err(|e| e.to_string())? + "\n"),
        Some(0o600),
    )
}

/// The state of a line this install inserted, read from the file each time:
/// `inserted` while the file holds the line, and `removed` once a read finds
/// it gone or finds no file. A read that fails leaves the recorded state, so
/// an unreadable file is never taken for a removal, and a line the person
/// adds back makes the state `inserted` again.
fn reinspect_line(record: &mut Value) {
    if !matches!(record["line"].as_str(), Some("inserted" | "removed")) {
        return;
    }
    let Some(config) = record["config"].as_str() else {
        return;
    };
    let present = match fs::read(config) {
        Ok(bytes) => install::has_line(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return,
    };
    record["line"] = json!(if present { "inserted" } else { "removed" });
}

pub(crate) fn inspect(home: Option<&Path>, endpoint: &Endpoint) -> Option<Result<Value, String>> {
    inspect_with(home, |record| crate::mux::rex::ui_report(endpoint, record))
}

/// `inspect`, with the server's report of `record` from `report`.
fn inspect_with(
    home: Option<&Path>,
    report: impl FnOnce(&Value) -> Result<Value, String>,
) -> Option<Result<Value, String>> {
    let mut record = match registration(home?) {
        Ok(Some(r)) if r["module"].as_str().is_some_and(|s| !s.is_empty()) => r,
        Ok(_) => return None,
        Err(e) => return Some(Err(e)),
    };
    reinspect_line(&mut record);
    Some(report(&record).map(|mut report| {
        if record["line"] == "removed" {
            report["confirmed"] = json!(false);
        } else if report["confirmed"] != true && stale(&record, &report) {
            report["stale"] = json!(true);
        }
        report
    }))
}

/// Whether the server runs an older text of the registered module, so a
/// reload is all it needs: About names the registered file, the eleven
/// actions loaded, and the file now holds the text for the recorded
/// launcher and state, as after a refresh rewrote it (the U6 review's F7).
fn stale(record: &Value, report: &Value) -> bool {
    let (Some(module), Some(launcher), Some(state)) = (
        record["module"].as_str(),
        record["launcher"].as_str(),
        record["state"].as_str(),
    ) else {
        return false;
    };
    report["actions_complete"] == true
        && report["about"]["module"]
            .as_str()
            .map(|loaded| loaded.strip_prefix('@').unwrap_or(loaded))
            == Some(module)
        && module_text(Path::new(launcher), Path::new(state))
            .is_ok_and(|text| fs::read_to_string(module).is_ok_and(|written| written == text))
}

/// The repair for a module the server loaded before a refresh rewrote it.
pub const RELOAD_REPAIR: &str = "Reload Rex's configuration with rex config reload.";

pub(crate) fn shortcuts(home: Option<&Path>, endpoint: &Endpoint) -> Value {
    shortcut_report(inspect(home, endpoint))
}

/// Activation's shortcut report: the keys only for a confirmed module.
fn shortcut_report(inspected: Option<Result<Value, String>>) -> Value {
    match inspected {
        Some(Ok(report)) if report["confirmed"] == true => {
            json!({"status":"rex_module", "path":report["module"], "keys":report["keys"]})
        }
        Some(Ok(report)) if report["stale"] == true => {
            json!({"status":"stale_module", "path":report["module"], "repair":RELOAD_REPAIR})
        }
        _ => json!({"status":"no_module", "repair":"Run sh scripts/install.sh."}),
    }
}

/// Palette errors are plain text, including a failure returned as an envelope.
pub fn error_line(error: &str) -> String {
    let decoded = serde_json::from_str::<Value>(error).ok();
    let message = decoded
        .as_ref()
        .and_then(|v| v["error"]["message"].as_str())
        .unwrap_or(error);
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;

/// A config start is accepted only when its socket peer is its parent.
/// Refusal performs no protocol request or state lookup. An accepted start
/// is keyed by the endpoint the server names itself by, which `prove` reads,
/// as each action's `endpoint()` is, however `REX_SERVER` spells the socket.
pub fn server_start_endpoint(
    inherited: &dyn crate::mux::home::Inherited,
    parent: u32,
    prove: impl FnOnce(&Path) -> Option<Endpoint>,
) -> Option<Endpoint> {
    let socket = crate::mux::home::rex_socket(inherited)?;
    let peer = inherited.peer(&socket).ok()?;
    (peer == parent && parent > 1)
        .then(|| prove(&socket))
        .flatten()
}

/// The endpoint the local Rex server at `socket` names itself by.
pub fn local_endpoint(socket: &Path) -> Option<Endpoint> {
    crate::mux::rex::local_endpoint(socket).ok()
}
