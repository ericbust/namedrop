//! Optional dashboard explanations; routing authority stays in SQLite.
use super::{ChannelRoutingState, MessageRow, SafeText, SpaceKey};
use crate::StateNamespace;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
    if file.metadata()?.len() + line.len() as u64 > MAX_BYTES {
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
    if let Some(internal) = internal {
        let note = Note {
            space: SpaceKey::of(namespace).as_str().to_owned(),
            message_id: message_id.to_owned(),
            reason: internal["reason"]
                .as_str()
                .map(|reason| SafeText::from_untrusted(reason, 60).text),
            router: internal["router"]
                .as_str()
                .map(|router| SafeText::from_untrusted(router, 80).text),
            router_failed: internal["router_failed"].as_bool().unwrap_or(false),
        };
        let _ = append(state, note.clone());
        apply_receipt(receipt, &note);
    } else if let Some(note) = notes(state, namespace).get(message_id) {
        apply_receipt(receipt, note);
    }
}

fn apply_receipt(receipt: &mut Value, note: &Note) {
    let channel = &mut receipt["channel"];
    if note.router_failed && channel["to"].as_array().is_some_and(Vec::is_empty) {
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
            if note.router_failed && channel.to.is_empty() {
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
