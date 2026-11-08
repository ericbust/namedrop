//! How large an agent's context is now, from the latest usage its own host
//! wrote to the session transcript.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{AgentConfigDirectories, AgentRuntime, AgentSessionId};

/// Claude can write a few hundred kilobytes of request records after its last
/// usage entry, so the read reaches back in doubling chunks until it finds one
/// or reaches the cap.
const FIRST_WINDOW: u64 = 64 * 1024;
const LAST_WINDOW: u64 = 8 * 1024 * 1024;

const DAY_MS: u64 = 86_400_000;

/// The context the session's host last reported, or `None` when its
/// transcript cannot be found, holds no usage yet, or was compacted since.
pub(crate) fn context_tokens(
    host: AgentRuntime,
    session: &AgentSessionId,
    cwd: Option<&str>,
    home: Option<&Path>,
    config: AgentConfigDirectories<'_>,
) -> Option<u64> {
    let session = session.as_str();
    // The id becomes a file name, so it may not name anything else.
    if session.is_empty()
        || !session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return None;
    }
    let path = match host {
        AgentRuntime::ClaudeCode => {
            let claude = config
                .claude
                .map(Path::to_owned)
                .or_else(|| Some(config.claude(home?)))?;
            claude_transcript(&claude, session, cwd)?
        }
        AgentRuntime::Codex => {
            let codex = config
                .codex
                .map(Path::to_owned)
                .or_else(|| Some(config.codex(home?)))?;
            codex_transcript(&codex, session)?
        }
    };
    latest_from_end(host, &open_transcript(&path)?)
}

fn claude_transcript(claude: &Path, session: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let projects = claude.join("projects");
    let file = format!("{session}.jsonl");
    // Claude names a project directory after its cwd with every character
    // outside [A-Za-z0-9] replaced by '-'. A long or moved cwd falls back to
    // the scan.
    if let Some(cwd) = cwd {
        let encoded: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let path = projects.join(encoded).join(&file);
        if path.is_file() {
            return Some(path);
        }
    }
    fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

fn codex_transcript(codex: &Path, session: &str) -> Option<PathBuf> {
    let sessions = codex.join("sessions");
    let suffix = format!("-{session}.jsonl");
    let in_day = |day: &Path| {
        fs::read_dir(day).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            (name.starts_with("rollout-") && name.ends_with(&suffix)).then(|| entry.path())
        })
    };
    // Codex files a rollout under the local day it started. A UUIDv7 id
    // carries that instant, so the day either side covers every time zone.
    if let Some(created) = uuid_v7_unix_ms(session) {
        let day = created / DAY_MS;
        for day in [day, day.saturating_sub(1), day + 1] {
            let (year, month, date) = civil_date(day);
            let directory = sessions
                .join(format!("{year:04}"))
                .join(format!("{month:02}"))
                .join(format!("{date:02}"));
            if let Some(path) = in_day(&directory) {
                return Some(path);
            }
        }
    }
    let children = |directory: &Path| -> Vec<PathBuf> {
        fs::read_dir(directory)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    };
    children(&sessions)
        .iter()
        .flat_map(|year| children(year))
        .flat_map(|month| children(&month))
        .find_map(|day| in_day(&day))
}

fn uuid_v7_unix_ms(session: &str) -> Option<u64> {
    let hex: String = session.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || hex.as_bytes()[12] != b'7' {
        return None;
    }
    u64::from_str_radix(&hex[..12], 16).ok()
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// civil_from_days).
fn civil_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, date)
}

/// A transcript path is built from the agent's own directories, so a symlink
/// or a FIFO there is refused instead of followed or blocked on.
fn open_transcript(path: &Path) -> Option<File> {
    if !path.is_absolute() {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

fn latest_from_end(host: AgentRuntime, file: &File) -> Option<u64> {
    let length = file.metadata().ok()?.len();
    let read_at = |bytes: &mut [u8], at| file.read_exact_at(bytes, at).ok();
    match latest_reading(host, length, FIRST_WINDOW, LAST_WINDOW, read_at)? {
        Reading::Tokens(tokens) => Some(tokens),
        Reading::Compacted => None,
    }
}

/// What a transcript line says about the context's size.
#[derive(Debug, PartialEq)]
enum Reading {
    /// A model turn's whole prompt.
    Tokens(u64),
    /// The host compacted the context. Until the next model turn the
    /// transcript holds only the old size, or a zero, so the size is unknown.
    Compacted,
}

/// The latest reading in the last `cap` bytes of a transcript `length` bytes
/// long. Chunks are read from the end, the first `first` bytes long and each
/// later one as long as all before it. The line a chunk cuts at its start is
/// carried into the next chunk, so each byte is read and parsed once. A line
/// the cap cuts is never parsed, and one still being written at the end is
/// not a whole JSON object, so it does not parse and is passed over.
fn latest_reading(
    host: AgentRuntime,
    length: u64,
    first: u64,
    cap: u64,
    read_at: impl Fn(&mut [u8], u64) -> Option<()>,
) -> Option<Reading> {
    let reading = match host {
        AgentRuntime::ClaudeCode => claude_reading,
        AgentRuntime::Codex => codex_reading,
    };
    let mut end = length;
    let mut cut = Vec::new();
    while end > 0 && length - end < cap {
        let start = end.saturating_sub((length - end).max(first));
        let mut bytes = vec![0; usize::try_from(end - start).ok()?];
        read_at(&mut bytes, start)?;
        bytes.extend_from_slice(&cut);
        let (cut_length, whole) = if start == 0 {
            (0, 0)
        } else {
            match bytes.iter().position(|byte| *byte == b'\n') {
                Some(newline) => (newline, newline + 1),
                None => (bytes.len(), bytes.len()),
            }
        };
        if let Some(found) = bytes[whole..]
            .rsplit(|byte| *byte == b'\n')
            .find_map(reading)
        {
            return Some(found);
        }
        bytes.truncate(cut_length);
        cut = bytes;
        end = start;
    }
    None
}

#[derive(Deserialize)]
struct ClaudeEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

/// A main-thread model turn's whole prompt, or the main thread's compaction
/// boundary. Claude writes all-zero usage on the entries right after a
/// compaction, and `<synthetic>` entries are local notices no model read, so
/// neither says how large the context is.
fn claude_reading(line: &[u8]) -> Option<Reading> {
    let entry: ClaudeEntry = serde_json::from_slice(line).ok()?;
    if entry.sidechain == Some(true) {
        return None;
    }
    if entry.kind.as_deref() == Some("system")
        && entry.subtype.as_deref() == Some("compact_boundary")
    {
        return Some(Reading::Compacted);
    }
    let message = entry.message?;
    if entry.kind.as_deref() != Some("assistant") || message.model.as_deref() == Some("<synthetic>")
    {
        return None;
    }
    let usage = message.usage?;
    let total = [
        usage.input_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
    ]
    .into_iter()
    .map(|tokens| tokens.unwrap_or(0))
    .sum::<u64>();
    (total > 0).then_some(Reading::Tokens(total))
}

#[derive(Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    last_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize)]
struct CodexUsage {
    input_tokens: Option<u64>,
}

/// The last model request's input, cached tokens included, or a compaction.
/// Codex also writes `token_count` events with `info: null`, which carry only
/// rate limits, and one with zero input right after a compaction.
fn codex_reading(line: &[u8]) -> Option<Reading> {
    let entry: CodexEntry = serde_json::from_slice(line).ok()?;
    if entry.kind.as_deref() == Some("compacted") {
        return Some(Reading::Compacted);
    }
    let payload = entry.payload?;
    if entry.kind.as_deref() != Some("event_msg") || payload.kind.as_deref() != Some("token_count")
    {
        return None;
    }
    let input = payload.info?.last_token_usage?.input_tokens?;
    (input > 0).then_some(Reading::Tokens(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(entries: &[serde_json::Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend(entry.to_string().as_bytes());
            bytes.push(b'\n');
        }
        bytes
    }

    fn from_slice(source: &[u8]) -> impl Fn(&mut [u8], u64) -> Option<()> + '_ {
        |bytes, at| {
            let at = usize::try_from(at).ok()?;
            bytes.copy_from_slice(source.get(at..at + bytes.len())?);
            Some(())
        }
    }

    fn scan(host: AgentRuntime, transcript: &[u8], first: u64, cap: u64) -> Option<Reading> {
        latest_reading(
            host,
            transcript.len() as u64,
            first,
            cap,
            from_slice(transcript),
        )
    }

    fn latest(host: AgentRuntime, transcript: &[u8]) -> Option<u64> {
        match scan(host, transcript, FIRST_WINDOW, LAST_WINDOW)? {
            Reading::Tokens(tokens) => Some(tokens),
            Reading::Compacted => None,
        }
    }

    fn claude_turn(input: u64, created: u64, read: u64) -> serde_json::Value {
        json!({"parentUuid":"p","isSidechain":false,"type":"assistant","message":{
            "model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"ok"}],
            "usage":{"input_tokens":input,"cache_creation_input_tokens":created,
                     "cache_read_input_tokens":read,"output_tokens":192,
                     "service_tier":"standard"}}})
    }

    fn codex_count(input: u64) -> serde_json::Value {
        json!({"timestamp":"2026-11-01T06:08:10.147Z","type":"event_msg","payload":{
            "type":"token_count","info":{
                "total_token_usage":{"input_tokens":956_164_041_u64,"cached_input_tokens":943_481_472_u64,"output_tokens":3_039_108},
                "last_token_usage":{"input_tokens":input,"cached_input_tokens":359_424,"output_tokens":3673},
                "model_context_window":475_000},
            "rate_limits":{"limit_id":"codex"}}})
    }

    #[test]
    fn claude_counts_the_latest_main_turns_whole_prompt() {
        let transcript = lines(&[
            claude_turn(2, 900, 60_000),
            claude_turn(2, 989, 64_469),
            json!({"type":"user","isSidechain":false,"message":{"role":"user","content":"next"}}),
            json!({"type":"last-prompt","lastPrompt":"next","sessionId":"s"}),
        ]);
        assert_eq!(
            latest(AgentRuntime::ClaudeCode, &transcript),
            Some(2 + 989 + 64_469)
        );
    }

    #[test]
    fn claude_skips_entries_that_do_not_measure_the_context() {
        let mut compacted = claude_turn(0, 0, 0);
        compacted["message"]["usage"]["iterations"] =
            json!([{"input_tokens":2,"cache_read_input_tokens":308_345}]);
        let mut sidechain = claude_turn(5, 0, 9_000);
        sidechain["isSidechain"] = json!(true);
        let mut synthetic = claude_turn(0, 0, 7_000);
        synthetic["message"]["model"] = json!("<synthetic>");
        let transcript = lines(&[
            json!({"type":"system","subtype":"compact_boundary"}),
            claude_turn(3, 100, 80_000),
            compacted,
            sidechain,
            synthetic,
        ]);
        assert_eq!(latest(AgentRuntime::ClaudeCode, &transcript), Some(80_103));
    }

    #[test]
    fn a_compaction_after_the_latest_usage_leaves_the_size_unknown() {
        let boundary = json!({"type":"system","subtype":"compact_boundary","isSidechain":false,
            "content":"Conversation compacted","level":"info",
            "compactMetadata":{"trigger":"auto","preTokens":757_456,"postTokens":36_039}});
        let mut claude = lines(&[
            claude_turn(2, 300, 757_000),
            boundary.clone(),
            json!({"type":"user","isSidechain":false,"message":{"role":"user","content":"summary"}}),
            json!({"type":"attachment","isSidechain":false}),
        ]);
        assert_eq!(latest(AgentRuntime::ClaudeCode, &claude), None);
        claude.extend(lines(&[claude_turn(2, 500, 62_000)]));
        assert_eq!(latest(AgentRuntime::ClaudeCode, &claude), Some(62_502));
        let mut side_boundary = boundary;
        side_boundary["isSidechain"] = json!(true);
        assert_eq!(
            latest(
                AgentRuntime::ClaudeCode,
                &lines(&[claude_turn(2, 300, 757_000), side_boundary])
            ),
            Some(757_302)
        );

        let mut codex = lines(&[
            codex_count(448_739),
            json!({"timestamp":"2026-11-06T21:19:38.480Z","ordinal":5300,"type":"compacted",
                   "payload":{"message":"","window_number":2,"replacement_history":[]}}),
            json!({"type":"turn_context","payload":{"cwd":"/work"}}),
            codex_count(0),
        ]);
        assert_eq!(latest(AgentRuntime::Codex, &codex), None);
        codex.extend(lines(&[codex_count(37_393)]));
        assert_eq!(latest(AgentRuntime::Codex, &codex), Some(37_393));
    }

    #[test]
    fn codex_reads_the_last_requests_input_past_null_info_events() {
        let transcript = lines(&[
            codex_count(359_668),
            codex_count(363_409),
            json!({"type":"event_msg","payload":{"type":"token_count","info":null,
                   "rate_limits":{"limit_id":"codex"}}}),
            json!({"type":"response_item","payload":{"type":"message","info":"not usage"}}),
        ]);
        assert_eq!(latest(AgentRuntime::Codex, &transcript), Some(363_409));
        assert_eq!(latest(AgentRuntime::ClaudeCode, &transcript), None);
    }

    #[test]
    fn a_line_the_cap_cuts_is_not_read_and_one_a_chunk_cuts_is_read_whole() {
        let whole = lines(&[codex_count(1_000)]);
        let length = whole.len() as u64;
        assert_eq!(
            scan(AgentRuntime::Codex, &whole, length, length),
            Some(Reading::Tokens(1_000))
        );
        for cap in 1..length {
            assert_eq!(
                scan(AgentRuntime::Codex, &whole, cap, cap),
                None,
                "a line cut at byte {} was read",
                length - cap
            );
        }

        let mut writing = lines(&[codex_count(1_000), codex_count(2_000)]);
        let compacted = lines(&[
            codex_count(1_000),
            json!({"type":"compacted","payload":{"message":""}}),
            codex_count(0),
        ]);
        writing.extend(&lines(&[codex_count(3_000)])[..40]);
        for first in 1..=writing.len().max(compacted.len()) as u64 {
            assert_eq!(
                scan(AgentRuntime::Codex, &writing, first, u64::MAX),
                Some(Reading::Tokens(2_000)),
                "first chunk {first}"
            );
            assert_eq!(
                scan(AgentRuntime::Codex, &compacted, first, u64::MAX),
                Some(Reading::Compacted),
                "first chunk {first}"
            );
        }
    }

    #[test]
    fn usage_far_from_the_end_is_found_and_past_the_cap_is_not() {
        let root =
            std::env::temp_dir().join(format!("namedrop-transcript-usage-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let request = json!({"type":"api-request-blob","message":"x".repeat(100 * 1024)});
        let mut near = lines(&[claude_turn(1, 2, 300_000)]);
        for _ in 0..3 {
            near.extend(lines(std::slice::from_ref(&request)));
        }
        let path = root.join("near.jsonl");
        fs::write(&path, &near).unwrap();
        assert!(near.len() as u64 > 3 * FIRST_WINDOW);
        assert_eq!(
            latest_from_end(AgentRuntime::ClaudeCode, &open_transcript(&path).unwrap()),
            Some(300_003)
        );
        let read = std::cell::Cell::new(0);
        let counting = |bytes: &mut [u8], at| {
            read.set(read.get() + bytes.len());
            from_slice(&near)(bytes, at)
        };
        assert_eq!(
            latest_reading(
                AgentRuntime::ClaudeCode,
                near.len() as u64,
                FIRST_WINDOW,
                LAST_WINDOW,
                counting
            ),
            Some(Reading::Tokens(300_003))
        );
        assert_eq!(read.get(), near.len(), "a byte was read twice");

        let mut far = lines(&[claude_turn(1, 2, 300_000)]);
        far.extend(vec![b' '; LAST_WINDOW as usize]);
        far.push(b'\n');
        fs::write(&path, &far).unwrap();
        assert_eq!(
            latest_from_end(AgentRuntime::ClaudeCode, &open_transcript(&path).unwrap()),
            None
        );

        std::os::unix::fs::symlink(&path, root.join("link.jsonl")).unwrap();
        assert!(open_transcript(&root.join("link.jsonl")).is_none());
        assert!(open_transcript(&root).is_none());
        assert!(open_transcript(Path::new("relative.jsonl")).is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_configured_host_directory_is_read_without_a_home() {
        let root =
            std::env::temp_dir().join(format!("namedrop-transcript-config-{}", std::process::id()));
        let claude_id = "0ec24369-ad9a-45c8-a30c-39a5fd652cea";
        let codex_id = "01a19dc7-2685-7540-a0d4-bee6e6878281";
        let place = |directory: PathBuf, file: String, transcript: Vec<u8>| {
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(file), transcript).unwrap();
        };
        for base in [root.join("configured"), root.join("home")] {
            let (claude, codex) = if base.ends_with("home") {
                (base.join(".claude"), base.join(".codex"))
            } else {
                (base.join("claude"), base.join("codex"))
            };
            place(
                claude.join("projects").join("-work"),
                format!("{claude_id}.jsonl"),
                lines(&[claude_turn(1, 2, 3_000)]),
            );
            place(
                codex.join("sessions/2026/11/02"),
                format!("rollout-2026-11-02T06-08-10-{codex_id}.jsonl"),
                lines(&[codex_count(4_000)]),
            );
        }
        let read = |home: Option<&Path>, configured: bool| {
            let claude = root.join("configured/claude");
            let codex = root.join("configured/codex");
            let config = AgentConfigDirectories {
                claude: configured.then_some(claude.as_path()),
                codex: configured.then_some(codex.as_path()),
            };
            (
                context_tokens(
                    AgentRuntime::ClaudeCode,
                    &AgentSessionId::new(claude_id),
                    None,
                    home,
                    config,
                ),
                context_tokens(
                    AgentRuntime::Codex,
                    &AgentSessionId::new(codex_id),
                    None,
                    home,
                    config,
                ),
            )
        };
        assert_eq!(read(None, true), (Some(3_003), Some(4_000)));
        assert_eq!(
            read(Some(&root.join("home")), false),
            (Some(3_003), Some(4_000))
        );
        assert_eq!(read(None, false), (None, None));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_codex_id_names_the_day_its_rollout_is_filed_under() {
        let created = uuid_v7_unix_ms("01a19dc7-2685-7540-a0d4-bee6e6878281").unwrap();
        assert_eq!(civil_date(created / DAY_MS), (2026, 11, 2));
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(11_016), (2000, 2, 29));
        assert_eq!(
            uuid_v7_unix_ms("0ec24369-ad9a-45c8-a30c-39a5fd652cea"),
            None
        );
        assert_eq!(uuid_v7_unix_ms("session-lead"), None);
    }
}
