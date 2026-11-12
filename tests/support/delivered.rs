#![allow(dead_code)]
//! Tests read what a delivery put in a pane as its envelope. A pointer prompt
//! names its attempt and location, so the helper rebuilds the envelope the way
//! dispatch and the hook do, whether or not the attempt has settled since. A
//! prompt that is already an envelope parses as itself.

use namedrop::{DeliveryAttemptId, IntegrationToolInvocation, StateNamespace, StateStore};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub fn envelope_of(prompt: &str) -> Value {
    if let Ok(envelope) = serde_json::from_str(prompt) {
        return envelope;
    }
    let nonce = prompt
        .strip_prefix("Namedrop message ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("neither an envelope nor a pointer: {prompt}"));
    let flag = |name: &str| quoted_after(prompt, &format!("{name} '"));
    let executable = prompt
        .split_once(": '")
        .and_then(|(_, rest)| rest.split_once('\''))
        .expect("a pointer names its executable")
        .0
        .to_owned();
    let state = flag("--plugin-state");
    let socket = flag("--herdr-socket");
    let environment = BTreeMap::from([
        ("HERDR_PLUGIN_STATE_DIR".to_owned(), state.clone()),
        ("HERDR_SOCKET_PATH".to_owned(), socket.clone()),
        ("HERDR_WORKSPACE_ID".to_owned(), flag("--herdr-workspace")),
        ("HERDR_PANE_ID".to_owned(), flag("--herdr-pane")),
    ]);
    let (worktree, attempt): (String, String) =
        rusqlite::Connection::open(Path::new(&state).join("namedrop.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT worktree_id, attempt_id FROM delivery_attempt_state
                 WHERE session_socket_id = ?1 AND pointer_nonce = ?2",
                [&socket, &nonce.to_owned()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|error| panic!("no attempt for pointer {nonce}: {error}"));
    let envelope = StateStore::open(Path::new(&state))
        .unwrap()
        .transaction(|transaction| {
            transaction.delivery_envelope(
                &StateNamespace::for_space(socket.clone(), worktree.clone()),
                &DeliveryAttemptId::new(attempt.clone()),
                IntegrationToolInvocation::new(executable.clone(), environment.clone()),
            )
        })
        .unwrap();
    serde_json::to_value(envelope).unwrap()
}

/// What a host submits for a delivery a test holds as its envelope: the pointer
/// dispatch typed. Anything else, a forged envelope included, goes through as it
/// is. A second presentation attests nothing in production, so a test that
/// repeats a delivery must call the hook with the pointer itself and say why.
pub fn as_typed<'a>(typed: impl IntoIterator<Item = &'a str>, prompt: &str) -> String {
    let Some(envelope) = serde_json::from_str::<Value>(prompt)
        .ok()
        .filter(|envelope| envelope["control"]["attempt_id"].is_string())
    else {
        return prompt.to_owned();
    };
    let Some(pointer) = typed
        .into_iter()
        .filter(|typed| typed.starts_with("Namedrop message "))
        .find(|typed| envelope_of(typed) == envelope)
    else {
        return prompt.to_owned();
    };
    assert!(
        !presented(pointer),
        "this delivery was already presented, and a repeat attests nothing: {pointer}"
    );
    pointer.to_owned()
}

fn presented(pointer: &str) -> bool {
    let nonce = pointer
        .strip_prefix("Namedrop message ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap();
    rusqlite::Connection::open(
        Path::new(&quoted_after(pointer, "--plugin-state '")).join("namedrop.sqlite3"),
    )
    .unwrap()
    .query_row(
        "SELECT pointer_presented_at_ms IS NOT NULL FROM delivery_attempt_state
         WHERE pointer_nonce = ?1",
        [nonce],
        |row| row.get(0),
    )
    .unwrap()
}

fn quoted_after(prompt: &str, opening: &str) -> String {
    prompt
        .split_once(opening)
        .and_then(|(_, rest)| rest.split_once('\''))
        .unwrap_or_else(|| panic!("a pointer names {opening}: {prompt}"))
        .0
        .to_owned()
}
