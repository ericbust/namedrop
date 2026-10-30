//! Tour presentations: short paged introductions shipped as repo files and
//! shown once per machine the first time Namedrop sees an agent pane.
//!
//! Content lives in `tours/<id>.md` so a maintainer can rewrite it without
//! touching code, and a major update ships a new file with a new id. The
//! once-only marker is keyed by that id and lives in the state directory, not
//! the checkout, so replacing or reinstalling the package never shows the same
//! presentation twice while a new id is shown exactly once.

use crate::{DiscoveredPane, DiscoveredWorktree, Host, LifecycleRuntime};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// The presentation shown automatically. Pointing this at a new id is how a
/// release shows a fresh tour to people who already saw the previous one.
pub const CURRENT_ID: &str = "welcome-1";

/// Manifest pane id and `--entrypoint` of the tour popup.
pub const ENTRYPOINT: &str = "tour";

pub const POPUP_WIDTH: &str = "72";
pub const POPUP_HEIGHT: &str = "24";

/// Columns a page body wraps to. Herdr is asked for 72 and may clamp the popup
/// narrower, so this keeps the margin the other popups keep.
pub const BODY_WIDTH: usize = 68;

/// Wrapped body lines one page may hold. The page title, the controls and the
/// popup border must still fit a 24-row popup, so a page never scrolls.
pub const MAX_BODY_LINES: usize = 16;

/// Every shipped presentation, by file name. Embedding keeps the tour working
/// from any install layout and lets the test suite check every page's fit.
const EMBEDDED: &[(&str, &str)] = &[("welcome-1.md", include_str!("../tours/welcome-1.md"))];

const MARKER_DIRECTORY: &str = "tours-shown";
const MAX_ID_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub id: String,
    pub title: String,
    pub pages: Vec<Page>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub title: String,
    /// Unwrapped paragraphs and verbatim lines, with an empty string for each
    /// blank line between them. [`wrapped_body`] lays them out for a width.
    pub body: Vec<String>,
    pub action: Option<PageAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAction {
    /// The page offers to name the pane that opened the tour.
    Name,
}

/// Parses one presentation file and enforces the fit rule, so a page that
/// would overflow the popup fails in tests instead of on someone's screen.
///
/// Body text follows the familiar Markdown habit: consecutive lines join into
/// one paragraph, a blank line separates paragraphs, and an indented line
/// (a command or a prompt to copy) stays on its own line exactly as written.
pub fn parse(source: &str) -> Result<Presentation, String> {
    let mut lines = source.lines();
    let mut id = None;
    let mut title = None;
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("presentation header line {line:?} is not `key: value`"))?;
        let slot = match key.trim() {
            "id" => &mut id,
            "title" => &mut title,
            other => return Err(format!("presentation header has unknown key {other:?}")),
        };
        if slot.replace(value.trim().to_owned()).is_some() {
            return Err(format!("presentation header repeats {:?}", key.trim()));
        }
    }
    if !closed {
        return Err("presentation header must end with a `---` line".to_owned());
    }
    let id = id.ok_or("presentation header is missing `id`")?;
    validate_id(&id)?;
    let title = title
        .filter(|title| !title.is_empty())
        .ok_or_else(|| format!("presentation {id} is missing a title"))?;
    reject_control_characters(&id, &title)?;

    let mut pages = Vec::new();
    let mut page = Vec::new();
    for line in lines {
        if line == "---" {
            pages.push(parse_page(&id, pages.len() + 1, &page)?);
            page.clear();
        } else {
            page.push(line);
        }
    }
    pages.push(parse_page(&id, pages.len() + 1, &page)?);
    Ok(Presentation { id, title, pages })
}

fn parse_page(id: &str, number: usize, lines: &[&str]) -> Result<Page, String> {
    let mut rest = lines.iter().skip_while(|line| line.trim().is_empty());
    let title = rest
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| format!("presentation {id} page {number} must start with `# <title>`"))?
        .to_owned();
    reject_control_characters(id, &title)?;
    if title.len() > BODY_WIDTH {
        return Err(format!(
            "presentation {id} page {number} title is wider than {BODY_WIDTH} columns"
        ));
    }

    let mut action = None;
    let mut body: Vec<String> = Vec::new();
    let mut paragraph: Option<String> = None;
    for line in rest {
        let line = line.trim_end();
        reject_control_characters(id, line)?;
        if line == "action: name" {
            if action.replace(PageAction::Name).is_some() {
                return Err(format!(
                    "presentation {id} page {number} repeats its action"
                ));
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("action:") {
            return Err(format!(
                "presentation {id} page {number} has unknown action {:?}; \
                 the only action is `action: name`",
                value.trim()
            ));
        }
        if line.is_empty() {
            body.extend(paragraph.take());
            if body.last().is_some_and(|last| !last.is_empty()) {
                body.push(String::new());
            }
        } else if line.starts_with(' ') {
            body.extend(paragraph.take());
            body.push(line.to_owned());
        } else {
            match &mut paragraph {
                Some(text) => {
                    text.push(' ');
                    text.push_str(line.trim_start());
                }
                None => paragraph = Some(line.to_owned()),
            }
        }
    }
    body.extend(paragraph);
    while body.last().is_some_and(String::is_empty) {
        body.pop();
    }

    let page = Page {
        title,
        body,
        action,
    };
    let height = wrapped_body(&page, BODY_WIDTH).len();
    if height > MAX_BODY_LINES {
        return Err(format!(
            "presentation {id} page {number} wraps to {height} lines at width {BODY_WIDTH}; \
             the popup holds {MAX_BODY_LINES}"
        ));
    }
    Ok(page)
}

/// Presentation text is written straight to a terminal, so escape sequences
/// in a file must never reach it.
fn reject_control_characters(id: &str, text: &str) -> Result<(), String> {
    if text.chars().any(char::is_control) {
        return Err(format!("presentation {id} contains a control character"));
    }
    Ok(())
}

/// Parses several presentations and rejects duplicate ids, because two files
/// sharing an id would also share one once-only marker.
pub fn parse_all(sources: &[&str]) -> Result<Vec<Presentation>, String> {
    let mut seen = HashSet::new();
    sources
        .iter()
        .map(|source| {
            let presentation = parse(source)?;
            if !seen.insert(presentation.id.clone()) {
                return Err(format!(
                    "presentation id {} is used more than once",
                    presentation.id
                ));
            }
            Ok(presentation)
        })
        .collect()
}

/// Every embedded presentation. Each file must be named `<id>.md`, so the
/// file a maintainer edits is the one `CURRENT_ID` names.
pub fn all() -> Result<Vec<Presentation>, String> {
    let sources = EMBEDDED
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    let presentations = parse_all(&sources)?;
    for ((file, _), presentation) in EMBEDDED.iter().zip(&presentations) {
        if file.strip_suffix(".md") != Some(presentation.id.as_str()) {
            return Err(format!(
                "tours/{file} declares id {}; name it tours/{}.md",
                presentation.id, presentation.id
            ));
        }
    }
    Ok(presentations)
}

pub fn current() -> Result<Presentation, String> {
    all()?
        .into_iter()
        .find(|presentation| presentation.id == CURRENT_ID)
        .ok_or_else(|| format!("no embedded presentation has id {CURRENT_ID}"))
}

/// Lays a page body out at `width` columns, the same way for the fit rule and
/// the popup, so a page that passes the tests renders without clipping.
/// Paragraphs break at the last space that fits; an indented line keeps its
/// indent on continuation lines so a long command still reads as one.
pub fn wrapped_body(page: &Page, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for entry in &page.body {
        let text = entry.trim_start();
        // An indent wider than half the popup would leave no room for the
        // line itself, so such a line is laid out flush instead.
        let indent = Some(&entry[..entry.len() - text.len()])
            .filter(|indent| indent.len() < width / 2)
            .unwrap_or_default();
        let limit = width - indent.len();
        let mut rest = text;
        loop {
            let (line, remainder) = split_line(rest, limit);
            lines.push(format!("{indent}{}", line.trim_end()));
            match remainder {
                Some(remainder) => rest = remainder,
                None => break,
            }
        }
    }
    lines
}

/// One wrapped line of `text` within `limit` bytes and what is left after it.
/// A byte limit also bounds displayed columns for UTF-8 text.
/// Mirrors the naming popup's wrapping: break after the last space when there
/// is one, otherwise cut at the limit, and always make progress. A word that
/// ends exactly at the limit stays on the line, since the space after it is
/// dropped at the break.
fn split_line(text: &str, limit: usize) -> (&str, Option<&str>) {
    if text.len() <= limit {
        return (text, None);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let window = if text.as_bytes()[end] == b' ' {
        end + 1
    } else {
        end
    };
    if let Some(space) = text[..window].rfind(' ').filter(|&space| space > 0) {
        end = space + 1;
    }
    if end == 0 {
        end = text.chars().next().map_or(text.len(), char::len_utf8);
    }
    let rest = text[end..].trim_start_matches(' ');
    (&text[..end], (!rest.is_empty()).then_some(rest))
}

/// Ids name marker files, so only the documented grammar is accepted: no
/// separators, no dots and nothing that could step outside the directory.
fn validate_id(id: &str) -> Result<(), String> {
    let mut bytes = id.bytes();
    let valid = id.len() <= MAX_ID_LEN
        && bytes
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "presentation id {id:?} must be lowercase letters, digits and hyphens, \
             start with a letter or digit, and be at most {MAX_ID_LEN} characters"
        ))
    }
}

fn marker_path(state_directory: &Path, id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    Ok(state_directory.join(MARKER_DIRECTORY).join(id))
}

pub fn is_shown(state_directory: &Path, id: &str) -> Result<bool, String> {
    let path = marker_path(state_directory, id)?;
    // A symlink planted at the marker path still counts as present; it is
    // never followed or written through.
    match fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to read the tour marker: {error}")),
    }
}

/// Claims the once-only marker. Only the call that creates the file gets
/// `true`, so two panes detected at the same moment cannot both open the tour.
pub fn mark_shown(state_directory: &Path, id: &str) -> Result<bool, String> {
    let path = marker_path(state_directory, id)?;
    let directory = state_directory.join(MARKER_DIRECTORY);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|error| format!("failed to create the tour marker directory: {error}"))?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(format!("failed to record the tour marker: {error}")),
    }
}

/// Clears the marker so the presentation shows again; `true` when one existed.
pub fn reset(state_directory: &Path, id: &str) -> Result<bool, String> {
    let path = marker_path(state_directory, id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to clear the tour marker: {error}")),
    }
}

/// Opens the tour popup for `target` and returns Herdr's JSON result.
pub fn open_popup<H, C, I>(
    runtime: &LifecycleRuntime<'_, H, C, I>,
    target: &DiscoveredPane,
) -> Result<Value, String>
where
    H: Host,
{
    let session = target
        .agent_session_id
        .as_ref()
        .map(|id| id.as_str())
        .unwrap_or_default();
    runtime
        .host
        .open_popup(&crate::Popup {
            entrypoint: ENTRYPOINT,
            width: POPUP_WIDTH,
            height: POPUP_HEIGHT,
            env: &[
                ("NAMEDROP_TARGET_WORKSPACE", target.workspace_id.clone()),
                ("NAMEDROP_TARGET_PANE", target.pane_id.clone()),
                ("NAMEDROP_TARGET_TERMINAL", target.terminal_id.clone()),
                ("NAMEDROP_TARGET_SESSION", session.to_owned()),
                (
                    "HERDR_PLUGIN_STATE_DIR",
                    runtime.state_directory.display().to_string(),
                ),
                ("HERDR_SOCKET_PATH", runtime.session_socket_id.to_owned()),
            ],
        })
        .map_err(String::from)
}

/// Shows the current presentation on the first agent pane Namedrop sees.
///
/// It runs inside activation and agent events, so it reports instead of
/// failing: a tour must never block naming or delivery. The marker is claimed
/// before the popup opens, which makes concurrent callers race on one file
/// instead of each opening a popup, and a failed open keeps the marker because
/// retrying a popup on every later event would be worse than missing it once;
/// the shortcut reopens it any time.
pub fn show_for_first_agent<H, C, I>(
    runtime: &LifecycleRuntime<'_, H, C, I>,
    discovered: &DiscoveredWorktree,
    preferred_pane_id: Option<&str>,
) -> Value
where
    H: Host,
{
    let not_shown =
        |reason: &str| json!({"shown": false, "presentation_id": CURRENT_ID, "reason": reason});
    let failed = |reason: &str, error: String| json!({"shown": false, "presentation_id": CURRENT_ID, "reason": reason, "error": error});

    let agents = || discovered.panes.iter().filter(|pane| pane.agent.is_some());
    let Some(target) = preferred_pane_id
        .and_then(|preferred| agents().find(|pane| pane.pane_id == preferred))
        .or_else(|| agents().next())
    else {
        return not_shown("no_agent_pane");
    };
    if let Err(error) = current() {
        return failed("presentation_error", error);
    }
    match is_shown(runtime.state_directory, CURRENT_ID) {
        Ok(true) => return not_shown("already_shown"),
        Ok(false) => {}
        Err(error) => return failed("marker_error", error),
    }
    match mark_shown(runtime.state_directory, CURRENT_ID) {
        Ok(true) => {}
        Ok(false) => return not_shown("already_shown"),
        Err(error) => return failed("marker_error", error),
    }
    match open_popup(runtime, target) {
        Ok(popup) => json!({
            "shown": true,
            "presentation_id": CURRENT_ID,
            "pane_id": target.pane_id,
            "popup": popup,
        }),
        Err(error) => json!({
            "shown": false,
            "presentation_id": CURRENT_ID,
            "reason": "open_failed",
            "error": error,
            "pane_id": target.pane_id,
        }),
    }
}
