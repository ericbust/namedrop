//! The relay: a mention to an agent on another machine goes from this
//! Namedrop to the Namedrop beside that machine's Rex server, which delivers
//! it there. Replies come back by this side's pull.

mod rex;
mod store;
mod wire;

use crate::{MessageId, StateNamespace, ThreadId};
use std::path::Path;
use store::{Outbound, RelayStore};

/// The hosts a `send` may name a remote recipient on.
pub(crate) trait RemoteHosts {
    /// The listed labels, or `None` when the send takes no remote recipient,
    /// so that `@a@b` reads exactly as it does outside Rex.
    fn listed(&self) -> Option<&[String]>;
}

/// Every reader of a message but a Rex home's `send`.
pub(crate) struct NoRemoteHosts;

impl RemoteHosts for NoRemoteHosts {
    fn listed(&self) -> Option<&[String]> {
        None
    }
}

/// The labels `rex hosts list --json` prints, byte for byte.
#[derive(Debug, Default)]
pub(crate) struct Hosts(Vec<String>);

impl RemoteHosts for Hosts {
    fn listed(&self) -> Option<&[String]> {
        Some(&self.0)
    }
}

impl Hosts {
    /// The host list of this machine's `rex`, as the relay's later calls
    /// read it.
    pub(crate) fn read() -> Result<Self, String> {
        let output = rex::run(&["hosts", "list", "--json"], None, rex::CALL)
            .map_err(|failure| format!("could not read your Rex hosts: {failure}"))?;
        let listed: serde_json::Value = serde_json::from_slice(&output)
            .map_err(|error| format!("could not read your Rex hosts: {error}"))?;
        Ok(Self(
            listed["hosts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["host"]["label"].as_str().map(str::to_owned))
                .collect(),
        ))
    }
}

/// Whether `label` can name a host in a mention: 1 to 64 ASCII letters,
/// digits, `-`, and `_`. Rex accepts more, but `-S` reads a value with a
/// port as an address, and a mention token ends at whitespace.
pub(crate) fn valid_label(label: &str) -> bool {
    (1..=64).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The refusal for a label that is not exactly a listed, valid one.
pub(crate) fn unlisted(label: &str, listed: &[String]) -> String {
    if listed.is_empty() {
        format!("{label} is not in your Rex hosts (rex hosts list lists none)")
    } else {
        format!(
            "{label} is not in your Rex hosts (rex hosts list): {}",
            listed.join(", ")
        )
    }
}

/// One remote recipient of a send: `@alias@label`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteName {
    pub(crate) alias: String,
    pub(crate) label: String,
}

impl RemoteName {
    /// The paneless author that stands for this agent in the sender's space.
    pub(crate) fn author(&self) -> crate::store::RelayAuthor {
        crate::store::RelayAuthor::new(
            crate::store::RelayOrigin::Label(&self.label),
            &self.alias,
            format!("{}@{}", self.alias, self.label),
        )
    }
}

/// Refuses a send whose `deliver` request for any remote recipient would
/// pass what the `rex` CLI sends, before anything is stored.
pub(crate) fn check_size(sender: &str, remote: &[RemoteName], text: &str) -> Result<(), String> {
    for name in remote {
        let size = wire::deliver_size(sender, &name.alias, text);
        if size > wire::REQUEST_LIMIT {
            return Err(format!(
                "the message to @{}@{} is too large to relay: its request is {size} bytes, \
                 and the limit is {}",
                name.alias,
                name.label,
                wire::REQUEST_LIMIT
            ));
        }
    }
    Ok(())
}

/// What a committed send hands the relay: the A message every remote
/// recipient's item carries.
pub(crate) struct Sent<'a> {
    pub namespace: &'a StateNamespace,
    pub sender: &'a str,
    pub message: &'a MessageId,
    pub thread: &'a ThreadId,
    pub text: &'a str,
    pub now_ms: i64,
}

/// Queues one `outbound` item per remote recipient of `sent`. A replayed
/// send queues the same keys, which are already there.
pub(crate) fn queue(state: &Path, sent: &Sent<'_>, remote: &[RemoteName]) -> Result<(), String> {
    let store = RelayStore::open(state, sent.now_ms).map_err(|error| error.to_string())?;
    for name in remote {
        store
            .queue(&Outbound {
                label: &name.label,
                message_id: sent.message.as_str(),
                recipient: &name.alias,
                endpoint: &sent.namespace.session_socket_id,
                space: sent.namespace.space_id.as_str(),
                sender: sent.sender,
                thread: sent.thread.as_str(),
                in_reply_to: None,
                text: sent.text,
                now_ms: sent.now_ms,
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// What the send's receipt says about each remote recipient.
pub(crate) fn present(response: &mut serde_json::Value, remote: &[RemoteName]) {
    if remote.is_empty() {
        return;
    }
    response["relayed"] = remote
        .iter()
        .map(|name| {
            serde_json::json!({
                "alias": name.alias,
                "host": name.label,
                "statement": format!(
                    "@{}@{}: queued for the Namedrop on {}, which delivers it there.",
                    name.alias, name.label, name.label
                ),
            })
        })
        .collect();
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::{
        cell::RefCell,
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    thread_local! {
        static PROGRAM: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// The `rex` this thread's relay runs: none outside `with_rex`, so no
    /// test runs the machine's own `rex`.
    pub(crate) fn program() -> PathBuf {
        PROGRAM
            .with(|program| program.borrow().clone())
            .unwrap_or_else(|| PathBuf::from("/nonexistent/namedrop-relay-tests/rex"))
    }

    /// A script `rex` in its own directory, whose body is `script`, and a
    /// `calls` file beside it where each run appends its arguments.
    pub(crate) struct Script {
        pub(crate) directory: PathBuf,
    }

    impl Script {
        pub(crate) fn new(script: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "namedrop-relay-rex-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&directory).unwrap();
            let rex = directory.join("rex");
            fs::write(
                &rex,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{script}\n",
                    directory.join("calls").display()
                ),
            )
            .unwrap();
            fs::set_permissions(&rex, fs::Permissions::from_mode(0o755)).unwrap();
            Self { directory }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.directory.join("calls"))
                .map(|calls| calls.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        }

        /// Runs `body` with this script as the thread's `rex`.
        pub(crate) fn run<T>(&self, body: impl FnOnce() -> T) -> T {
            PROGRAM.with(|program| *program.borrow_mut() = Some(self.directory.join("rex")));
            let result = body();
            PROGRAM.with(|program| *program.borrow_mut() = None);
            result
        }
    }

    impl Drop for Script {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    /// A `rex` whose `hosts list --json` prints `labels` as Rex does.
    pub(crate) fn hosts(labels: &[&str]) -> Script {
        let hosts: Vec<_> = labels
            .iter()
            .map(|label| {
                serde_json::json!({"host": {"id": format!("host:{label}"), "label": label,
                    "endpoints": ["127.0.0.1:1"]}, "from": "home", "held": false})
            })
            .collect();
        let listed = serde_json::json!({
            "home": {"label": "a.s", "endpoint": "unix:///tmp/a.sock", "this_machine": false,
                "reached": true},
            "hosts": hosts,
        });
        Script::new(&format!("cat <<'EOF'\n{listed}\nEOF"))
    }

    /// Runs `body` with a `rex` whose host list is `labels`.
    pub(crate) fn with_hosts<T>(labels: &[&str], body: impl FnOnce() -> T) -> T {
        hosts(labels).run(body)
    }
}

#[cfg(test)]
mod tests;
