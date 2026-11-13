//! Rex's JSON in domain types, Rex faults as `MuxError`, method names, and
//! the screen's ANSI in the port's shape.

use super::control::Fault;
use crate::mux::{Gone, MuxError};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// The creator of terminal blocks, whose methods take this prefix.
pub(super) const TERMINAL: &str = "com.superlogical.terminal";

pub(super) fn terminal_method(name: &str) -> String {
    format!("{TERMINAL}.{name}")
}

/// A block method's arguments: the block, and the method's own under `args`.
pub(super) fn block_call(session: &str, block: &str, args: Value) -> Value {
    json!({"session_id": session, "block_id": block, "args": args})
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct Proc {
    pub pid: u32,
    pub name: String,
    /// The executable, with symlinks resolved.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// `process`: the terminal's child, its foreground, and the processes
/// between them, nearest first, ending with the child.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(super) struct ProcessReply {
    #[serde(default)]
    pub child: Option<Proc>,
    #[serde(default)]
    pub foreground: Option<Proc>,
    #[serde(default)]
    pub ancestors: Option<Vec<Proc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct Owner {
    pub pid: u32,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct StatusRecord {
    #[serde(default)]
    pub id: Option<String>,
    pub state: String,
    #[serde(default)]
    pub app: Option<String>,
    /// The terminal's foreground process when the report arrived.
    #[serde(default)]
    pub owner: Option<Owner>,
}

#[derive(Deserialize)]
struct Records {
    records: Vec<StatusRecord>,
}

/// The root record of a `program_status` reply: the one with no `id`.
pub(super) fn root_record(payload: Value) -> Result<Option<StatusRecord>, MuxError> {
    let Records { records } = parse(payload)?;
    Ok(records
        .into_iter()
        .find(|record| record.id.as_deref().is_none_or(str::is_empty)))
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct SessionEntry {
    pub session_id: String,
    pub label: String,
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Option<Vec<SessionEntry>>,
}

pub(super) fn sessions(payload: Value) -> Result<Vec<SessionEntry>, MuxError> {
    let Sessions { sessions } = parse(payload)?;
    Ok(sessions.unwrap_or_default())
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct BlockEntry {
    pub block_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub creator_name: Option<String>,
    /// Which machine and account the block runs as.
    #[serde(default)]
    pub host_id: Option<String>,
    /// Left out for a detached block.
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Option<Vec<BlockEntry>>,
}

pub(super) fn blocks(payload: Value) -> Result<Vec<BlockEntry>, MuxError> {
    let Blocks { blocks } = parse(payload)?;
    Ok(blocks.unwrap_or_default())
}

/// The host of `block` in a `session.attach` view, which lists every placed
/// block, or `None` for a block the view does not hold.
pub(super) fn view_host(view: &Value, block: &str) -> Option<Option<String>> {
    view["view"]["windows"]
        .as_array()?
        .iter()
        .filter_map(|window| window["layers"].as_array())
        .flatten()
        .filter_map(|layer| layer["blocks"].as_array())
        .flatten()
        .find(|entry| entry["block_id"] == block)
        .map(|entry| entry["host_id"].as_str().map(str::to_owned))
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Target {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub label: String,
}

#[derive(Deserialize)]
struct Resolved {
    results: Vec<ResolvedEntry>,
}

#[derive(Deserialize)]
struct ResolvedEntry {
    status: String,
    #[serde(default)]
    target: Option<Target>,
    #[serde(default)]
    reason: String,
}

pub(super) fn resolve_request(kind: &str, id: &str) -> Value {
    json!({"kinds": [kind], "queries": [id]})
}

/// The one target a `target.resolve` of a full ID names, or `Gone` when it
/// names nothing.
pub(super) fn target(payload: Value, gone: Gone, id: &str) -> Result<Target, MuxError> {
    let Resolved { results } = parse(payload)?;
    match results.into_iter().next() {
        Some(ResolvedEntry {
            status,
            target: Some(target),
            ..
        }) if status == "resolved" => Ok(target),
        Some(entry) if entry.status == "ambiguous" => Err(MuxError::Protocol(format!(
            "Rex resolved {id} to more than one target: {}",
            entry.reason
        ))),
        Some(entry) if entry.reason.is_empty() => {
            Err(MuxError::Gone(gone, format!("Rex has no {id}")))
        }
        Some(entry) => Err(MuxError::Gone(
            gone,
            format!("Rex has no {id}: {}", entry.reason),
        )),
        None => Err(MuxError::Protocol(format!(
            "Rex answered no result for {id}"
        ))),
    }
}

pub(super) fn parse<T: DeserializeOwned>(payload: Value) -> Result<T, MuxError> {
    serde_json::from_value(payload).map_err(|error| MuxError::Protocol(error.to_string()))
}

/// A fault in the port's words. `gone` says what a `not_found` names.
pub(super) fn mux_error(fault: Fault, gone: Gone) -> MuxError {
    match fault {
        Fault::Rex { code, message, .. } => match code.as_str() {
            "session_not_found" => MuxError::Gone(Gone::Space, message),
            "not_found" => MuxError::Gone(gone, message),
            "timeout" => MuxError::Timeout(message),
            "too_large" => MuxError::TooLarge(message),
            _ => MuxError::Unavailable(format!("{message} ({code})")),
        },
        Fault::Timeout(limit) => MuxError::Timeout(format!(
            "Rex request timed out after {}ms",
            limit.as_millis()
        )),
        Fault::TooLarge => MuxError::TooLarge("Rex read exceeded its output limit".into()),
        Fault::Cancelled => MuxError::Unavailable("Rex read cancelled".into()),
        Fault::Closed(text) | Fault::Allowance(text) | Fault::NotLocal(text) => {
            MuxError::Unavailable(text)
        }
    }
}

/// Rex's `vt` screen in the port's ANSI shape. Each `ESC[nC` becomes n
/// spaces, because Rex writes a run of blank cells, a tab among them, that
/// way and the port's reader drops every CSI but SGR. SGR and OSC 8 links
/// pass through, and `\r\n` becomes `\n`. Any other escape sequence or
/// control character is refused, and the error names it, so a screen the
/// reader would misread is never read at all.
pub(crate) fn port_ansi(vt: &str) -> Result<String, String> {
    /// Wider than any screen, so a larger count is not a blank run.
    const WIDEST_RUN: usize = 4096;
    let refuse = |sequence: &str| Err(format!("{sequence:?}"));
    let mut screen = String::with_capacity(vt.len());
    let mut characters = vt.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => match characters.next() {
                Some('[') => {
                    let mut parameters = String::new();
                    let last = loop {
                        match characters.next() {
                            Some(next) if ('\u{40}'..='\u{7e}').contains(&next) => break next,
                            Some(next) => parameters.push(next),
                            None => return refuse(&format!("\u{1b}[{parameters}")),
                        }
                    };
                    let sgr = |c: char| c.is_ascii_digit() || c == ';' || c == ':';
                    let count = match parameters.as_str() {
                        "" => Some(1),
                        digits => digits.parse().ok().filter(|count| *count <= WIDEST_RUN),
                    };
                    match (last, count) {
                        ('m', _) if parameters.chars().all(sgr) => {
                            screen.push_str(&format!("\u{1b}[{parameters}m"));
                        }
                        ('C', Some(count)) => screen.extend(std::iter::repeat_n(' ', count)),
                        _ => return refuse(&format!("\u{1b}[{parameters}{last}")),
                    }
                }
                Some(']') => {
                    let mut body = String::new();
                    let terminator = loop {
                        match characters.next() {
                            Some('\u{7}') => break "\u{7}",
                            Some('\u{1b}') if characters.next_if_eq(&'\\').is_some() => {
                                break "\u{1b}\\";
                            }
                            Some('\u{1b}') | None => return refuse(&format!("\u{1b}]{body}")),
                            Some(next) => body.push(next),
                        }
                    };
                    if !body.starts_with("8;") {
                        return refuse(&format!("\u{1b}]{body}{terminator}"));
                    }
                    screen.push_str(&format!("\u{1b}]{body}{terminator}"));
                }
                Some(next) => return refuse(&format!("\u{1b}{next}")),
                None => return refuse("\u{1b}"),
            },
            '\r' if characters.peek() == Some(&'\n') => {}
            '\n' => screen.push('\n'),
            control if control.is_control() => return refuse(&control.to_string()),
            character => screen.push(character),
        }
    }
    Ok(screen)
}
