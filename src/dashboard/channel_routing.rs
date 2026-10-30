use super::{ChannelRoutingState, MessageRow, SafeText, SpaceKey};
use crate::StateNamespace;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_BYTES: u64 = 512 * 1024;
const MAX_LINE: usize = 1024;
const MAX_RECORDS: usize = 1024;

#[derive(Clone, Deserialize, Serialize)]
struct Note {
    space: String,
    message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    router: Option<String>,
    router_failed: bool,
}

fn path(state: &Path) -> PathBuf {
    state.join("dashboard/channel-routing.jsonl")
}

pub(crate) fn revision(state: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::symlink_metadata(path(state)).ok()?;
    (!metadata.file_type().is_symlink()).then_some((metadata.modified().ok()?, metadata.len()))
}

fn open_owned(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other("unsafe channel routing sidecar"));
    }
    Ok(file)
}

fn lock(state: &Path, exclusive: bool) -> io::Result<File> {
    let directory = state.join("dashboard");
    if exclusive {
        fs::create_dir_all(&directory)?;
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("unsafe dashboard directory"));
    }
    let file = open_owned(&directory.join("channel-routing.lock"), true)?;
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

fn read_notes(state: &Path) -> io::Result<Vec<Note>> {
    let mut file = open_owned(&path(state), false)?;
    if file.metadata()?.len() > MAX_BYTES {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Ok(Vec::new());
    }
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| line.len() <= MAX_LINE)
        .filter_map(|line| serde_json::from_slice::<Note>(line).ok())
        .filter(|note| note.space.len() == 20 && note.message_id.len() <= 256)
        .collect())
}

fn notes(state: &Path, namespace: &StateNamespace) -> BTreeMap<String, Note> {
    let Ok(_lock) = lock(state, false) else {
        return BTreeMap::new();
    };
    let space = SpaceKey::of(namespace);
    read_notes(state)
        .unwrap_or_default()
        .into_iter()
        .filter(|note| note.space == space.as_str())
        .map(|note| (note.message_id.clone(), note))
        .collect()
}

fn append(state: &Path, note: Note) -> io::Result<()> {
    let _lock = lock(state, true)?;
    let mut line = serde_json::to_vec(&note)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Ok(());
    }
    let mut file = open_owned(&path(state), true)?;
    let length = file.metadata()?.len();
    let mut tail = *b"\n";
    if length > 0 {
        file.read_exact_at(&mut tail, length - 1)?;
    }
    let needs_newline = tail[0] != b'\n';
    if length + line.len() as u64 + u64::from(needs_newline) > MAX_BYTES {
        let mut retained = read_notes(state).unwrap_or_default();
        retained.push(note);
        let start = retained.len().saturating_sub(MAX_RECORDS / 2);
        let mut compact = Vec::new();
        for note in &retained[start..] {
            let record = serde_json::to_vec(note)?;
            if compact.len() + record.len() + 1 > MAX_BYTES as usize {
                break;
            }
            compact.extend(record);
            compact.push(b'\n');
        }
        file.set_len(0)?;
        file.write_all(&compact)?;
    } else {
        use std::io::Seek;
        file.seek(io::SeekFrom::End(0))?;
        if needs_newline {
            file.write_all(b"\n")?;
        }
        file.write_all(&line)?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct AttemptRecord {
    call_id: String,
    request_id: String,
    space: String,
    timestamp_ms: u64,
    #[serde(flatten)]
    metrics: crate::agent_status_model::channel_router::RouterAttempt,
}

pub(crate) fn record_attempt(
    state: &Path,
    namespace: &StateNamespace,
    request_id: &str,
    call_id: &str,
    metrics: crate::agent_status_model::channel_router::RouterAttempt,
) {
    let record = AttemptRecord {
        call_id: call_id.to_owned(),
        request_id: request_id.to_owned(),
        space: SpaceKey::of(namespace).as_str().to_owned(),
        timestamp_ms: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        metrics,
    };
    if append_attempt(state, &record).is_err() {
        eprintln!("dashboard: channel router attempt journal unavailable");
    }
}

fn append_attempt(state: &Path, record: &AttemptRecord) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let _lock = lock(state, true)?;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(io::Error::other("channel router attempt record too long"));
    }
    let mut file = open_owned(&state.join("dashboard/channel-router-attempts.jsonl"), true)?;
    let length = file.metadata()?.len();
    let mut tail = *b"\n";
    if length > 0 {
        file.read_exact_at(&mut tail, length - 1)?;
    }
    let needs_newline = tail[0] != b'\n';
    if length + line.len() as u64 + u64::from(needs_newline) > MAX_BYTES {
        file.seek(SeekFrom::Start(length.saturating_sub(MAX_BYTES / 2)))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_BYTES / 2 + 1)
            .read_to_end(&mut bytes)?;
        let mut records: Vec<_> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| line.len() <= MAX_LINE)
            .filter_map(|line| serde_json::from_slice::<AttemptRecord>(line).ok())
            .collect();
        records.push(record.clone());
        let start = records.len().saturating_sub(MAX_RECORDS / 2);
        let mut compact = Vec::new();
        for record in &records[start..] {
            let encoded = serde_json::to_vec(record)?;
            if compact.len() + encoded.len() + 1 > MAX_BYTES as usize {
                return Err(io::Error::other(
                    "channel router attempt retention exceeded",
                ));
            }
            compact.extend(encoded);
            compact.push(b'\n');
        }
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(&compact)?;
    } else {
        file.seek(SeekFrom::End(0))?;
        if needs_newline {
            file.write_all(b"\n")?;
        }
        file.write_all(&line)?;
    }
    Ok(())
}

pub(crate) fn finish_post(state: &Path, namespace: &StateNamespace, receipt: &mut Value) {
    let internal = receipt
        .as_object_mut()
        .and_then(|receipt| receipt.remove("_channel_routing_note"));
    if receipt.get("channel").is_none() {
        return;
    }
    let Some(message_id) = receipt["message_id"].as_str() else {
        return;
    };
    let marked = crate::StateStore::open_read_only(state)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|tx| {
                    tx.channel_routing_state(namespace, &crate::MessageId::new(message_id))
                })
                .ok()
                .flatten()
        })
        .is_some();
    if let Some(internal) = internal {
        let note = Note {
            space: SpaceKey::of(namespace).as_str().to_owned(),
            message_id: message_id.to_owned(),
            reason: internal["reason"]
                .as_str()
                .map(|reason| SafeText::from_untrusted(reason, 60).text)
                .filter(|reason| !reason.trim().is_empty()),
            router: internal["router"]
                .as_str()
                .map(|router| SafeText::from_untrusted(router, 80).text),
            router_failed: internal["router_failed"].as_bool().unwrap_or(false),
        };
        let _ = append(state, note.clone());
        apply_receipt(receipt, &note, marked);
    } else if let Some(note) = notes(state, namespace).get(message_id) {
        apply_receipt(receipt, note, marked);
    }
}

fn apply_receipt(receipt: &mut Value, note: &Note, marked: bool) {
    let channel = &mut receipt["channel"];
    if !marked && note.router_failed && channel["to"].as_array().is_some_and(Vec::is_empty) {
        channel["state"] = "router_failed".into();
    }
    if let Some(reason) = &note.reason {
        channel["reason"] = SafeText::from_untrusted(reason, 60).text.into();
    }
    if let Some(router) = &note.router {
        channel["router"] = SafeText::from_untrusted(router, 80).text.into();
    }
}

pub(crate) fn merge(state: &Path, namespace: &StateNamespace, messages: &mut [MessageRow]) {
    let notes = notes(state, namespace);
    for message in messages {
        if let Some(channel) = &mut message.channel
            && let Some(note) = notes.get(&message.message_id)
        {
            if !channel.durable && note.router_failed && channel.to.is_empty() {
                channel.state = ChannelRoutingState::RouterFailed;
            }
            channel.reason = note
                .reason
                .as_deref()
                .map(|reason| SafeText::from_untrusted(reason, 60).text);
            channel.router = note
                .router
                .as_deref()
                .map(|router| SafeText::from_untrusted(router, 80).text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_status_model::channel_router::{RouterAttempt, RouterHost, RouterOutcome};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "namedrop-router-journal-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn attempt() -> RouterAttempt {
        RouterAttempt {
            host: RouterHost::Claude,
            model: "claude-haiku-4-5".to_owned(),
            outcome: RouterOutcome::Timeout,
            duration_ms: 8_000,
            num_turns: None,
            input_tokens: None,
            output_tokens: None,
            usage_complete: false,
            provider_usd: None,
        }
    }

    #[test]
    fn attempt_retention_preserves_latest_records_and_never_changes_message_notes() {
        let state = directory();
        let namespace = StateNamespace::for_space("synthetic", "w1");
        let mut receipt = serde_json::json!({"message_id":"post-1","channel":{"to":[]},"_channel_routing_note":{"reason":"statement","router":"claude-haiku-4-5","router_failed":false}});
        finish_post(&state, &namespace, &mut receipt);
        let note = fs::read(path(&state)).unwrap();
        let note_revision = revision(&state);
        let journal = state.join("dashboard/channel-router-attempts.jsonl");
        for index in 0..2_000 {
            record_attempt(
                &state,
                &namespace,
                "reused-request",
                &format!("call-{index}"),
                attempt(),
            );
        }
        assert!(fs::metadata(&journal).unwrap().len() <= MAX_BYTES);
        assert_eq!(
            fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let data = fs::read_to_string(&journal).unwrap();
        let records: Vec<AttemptRecord> = data
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.last().unwrap().call_id, "call-1999");
        assert_ne!(records.first().unwrap().call_id, "call-0");
        assert!(
            records
                .iter()
                .all(|record| record.request_id == "reused-request")
        );
        assert_eq!(fs::read(path(&state)).unwrap(), note);
        assert_eq!(revision(&state), note_revision);
        let mut replay = serde_json::json!({"message_id":"post-1","channel":{"to":[]}});
        finish_post(&state, &namespace, &mut replay);
        assert_eq!(replay["channel"], receipt["channel"]);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn attempt_journal_refuses_symlinks_and_public_files_without_affecting_notes() {
        let state = directory();
        let namespace = StateNamespace::for_space("synthetic", "w1");
        fs::create_dir_all(state.join("dashboard")).unwrap();
        let journal = state.join("dashboard/channel-router-attempts.jsonl");
        let target = state.join("private-target");
        fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, &journal).unwrap();
        record_attempt(&state, &namespace, "request", "call-1", attempt());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        fs::remove_file(&journal).unwrap();
        fs::write(&journal, "untouched").unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
        record_attempt(&state, &namespace, "request", "call-2", attempt());
        assert_eq!(fs::read_to_string(&journal).unwrap(), "untouched");
        let mut receipt = serde_json::json!({"message_id":"post-1","channel":{"to":[]},"_channel_routing_note":{"reason":"statement","router":"claude-haiku-4-5","router_failed":false}});
        finish_post(&state, &namespace, &mut receipt);
        assert_eq!(receipt["channel"]["reason"], "statement");
        assert!(path(&state).exists());
        fs::remove_dir_all(state).unwrap();
    }
}
