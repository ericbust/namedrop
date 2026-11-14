//! `namedrop relay accept`, on B: one `namedrop.relay` call that B's bridge
//! admitted, decided and stored, with B's outbox for the caller.

use super::{
    store::{InboundKey, RelayStore, RemoteThread},
    wire::{Answer, Item, Op, Outcome, PAGE_LIMIT, Request, Status, TEXT_LIMIT},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, StateNamespace, StateStore,
    ThreadId,
    mux::Endpoint,
    store::{RelayAuthor, RelayOrigin, Relayed},
};
use std::path::Path;

/// B keeps replies for a sender only while it holds fewer than this.
const OUTBOX_LIMIT: usize = 10_000;

/// Decides the call `input` holds for the server `host_id` behind
/// `endpoint`, and calls `wake` for the space of an item it accepts.
pub(crate) fn accept(
    state: &Path,
    endpoint: &Endpoint,
    host_id: &str,
    input: &[u8],
    wake: &mut dyn FnMut(&crate::SpaceId),
) -> Answer {
    let now_ms = super::now_ms();
    let refused = |reason: String| Answer {
        host_id: host_id.to_owned(),
        can_reply: false,
        result: Some(Outcome {
            id: String::new(),
            to: String::new(),
            status: Status::Refused,
            reason: Some(reason),
        }),
        outbox: Vec::new(),
        more: false,
    };
    let request: Request = match serde_json::from_slice(input) {
        Ok(request) => request,
        Err(error) => return refused(format!("the request is not a relay request: {error}")),
    };
    if let Err(field) = validate(&request) {
        let mut answer = refused(format!("{field} is not valid"));
        if let (Some(result), Some(item)) = (&mut answer.result, &request.item) {
            result.id.clone_from(&item.id);
            result.to.clone_from(&item.to);
        }
        return answer;
    }
    let store = match RelayStore::open(state, now_ms) {
        Ok(store) => store,
        Err(error) => return retry(host_id, request.item.as_ref(), &error.to_string()),
    };
    let result = match (&request.op, &request.item) {
        (Op::Deliver, Some(item)) => {
            match deliver(state, endpoint, &store, &request, item, now_ms, wake) {
                Ok(outcome) => Some(outcome),
                Err(error) => return retry(host_id, Some(item), &error),
            }
        }
        (Op::Deliver, None) => return refused("item is not valid".to_owned()),
        (Op::Collect, _) => None,
    };
    match store.outbox_page(&request.from.host_id, request.ack_through, PAGE_LIMIT) {
        Ok((outbox, more, kept)) => Answer {
            host_id: host_id.to_owned(),
            can_reply: kept < OUTBOX_LIMIT,
            result,
            outbox,
            more,
        },
        Err(error) => retry(host_id, request.item.as_ref(), &error.to_string()),
    }
}

/// An answer for a fault of B's own, after which A sends the item again.
fn retry(host_id: &str, item: Option<&Item>, reason: &str) -> Answer {
    Answer {
        host_id: host_id.to_owned(),
        can_reply: true,
        result: item.map(|item| Outcome {
            id: item.id.clone(),
            to: item.to.clone(),
            status: Status::Retry,
            reason: Some(reason.to_owned()),
        }),
        outbox: Vec::new(),
        more: false,
    }
}

/// Every field a direct client could make up, checked before anything is
/// read or stored. The error names the field.
fn validate(request: &Request) -> Result<(), &'static str> {
    let id = |value: &str| {
        (1..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
    };
    if !id(&request.expect_host_id) {
        return Err("expect_host_id");
    }
    if !id(&request.from.host_id) {
        return Err("from.host_id");
    }
    if crate::validate_alias(&request.from.name).is_err() {
        return Err("from.name");
    }
    if !super::valid_label(&request.from.host) {
        return Err("from.host");
    }
    let Some(item) = &request.item else {
        return Ok(());
    };
    if !id(&item.id) {
        return Err("item.id");
    }
    if crate::validate_alias(&item.to).is_err() {
        return Err("item.to");
    }
    if !id(&item.thread) {
        return Err("item.thread");
    }
    if item.in_reply_to.as_deref().is_some_and(|value| !id(value)) {
        return Err("item.in_reply_to");
    }
    if item.text.len() > TEXT_LIMIT {
        return Err("item.text");
    }
    Ok(())
}

/// The item's outcome: the stored one for a known key, or a new one stored
/// with its key. A refusal that depends on B's state is stored too, so a
/// retry never changes the answer.
fn deliver(
    state: &Path,
    endpoint: &Endpoint,
    store: &RelayStore,
    request: &Request,
    item: &Item,
    now_ms: i64,
    wake: &mut dyn FnMut(&crate::SpaceId),
) -> Result<Outcome, String> {
    let key = InboundKey {
        host_id: &request.from.host_id,
        message_id: &item.id,
        recipient: &item.to,
    };
    if let Some(mut stored) = store.inbound(&key).map_err(|error| error.to_string())? {
        if stored.status == Status::Accepted {
            stored.status = Status::Duplicate;
        }
        return Ok(stored);
    }
    let outcome = |status, reason: Option<String>| Outcome {
        id: item.id.clone(),
        to: item.to.clone(),
        status,
        reason,
    };
    let mut main = StateStore::open(state).map_err(|error| error.to_string())?;
    let space = match place(&mut main, store, endpoint, request, item)? {
        Ok(space) => space,
        Err((status, reason)) => {
            let refusal = outcome(status, Some(reason));
            store
                .record_inbound(&key, &refusal, None, now_ms)
                .map_err(|error| error.to_string())?;
            return Ok(refusal);
        }
    };
    let namespace = StateNamespace::for_space(&endpoint.key, space.as_str());
    let thread = RemoteThread {
        host_id: &request.from.host_id,
        thread: &item.thread,
        endpoint: &endpoint.key,
        space: space.as_str(),
        sender: &request.from.name,
        display_host: &request.from.host,
    };
    let ids = crate::ProcessIds::default();
    let in_reply_to = item.in_reply_to.as_deref().map(MessageId::new);
    let local_thread = match &in_reply_to {
        Some(message) => main
            .transaction(|transaction| transaction.message_place(&endpoint.key, message))
            .map_err(|error| error.to_string())?
            .map(|(_, thread)| thread),
        None => store
            .remote_thread(&thread)
            .map_err(|error| error.to_string())?
            .map(ThreadId::new),
    }
    .unwrap_or_else(|| ThreadId::new(ids.next_id("thread")));
    let author = RelayAuthor::new(
        RelayOrigin::Host(&request.from.host_id),
        &request.from.name,
        format!("{}@{}", request.from.name, request.from.host),
    );
    let request_id = format!("relay:{}:{}:{}", request.from.host_id, item.id, item.to);
    let attribution = MutationAttribution::new(
        ActorKind::AgentSession,
        author.address_id.as_str(),
        u64::try_from(now_ms).unwrap_or_default(),
    );
    let inserted = main.transaction(|transaction| {
        let recipient = transaction.resolve_routing_alias(&namespace, &item.to)?;
        transaction.insert_relayed_message(
            &namespace,
            &Relayed {
                author: &author,
                request_id: request_id.clone(),
                thread: &local_thread,
                message_id: MessageId::new(ids.next_id("message")),
                delivery_id: DeliveryId::new(ids.next_id("delivery")),
                in_reply_to: in_reply_to.as_ref(),
                recipient: &recipient,
                text: &item.text,
            },
            &attribution,
        )
    });
    let receipt = match inserted {
        Ok(receipt) => receipt,
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => {
            let refusal = outcome(Status::Refused, Some(error.to_string()));
            store
                .record_inbound(&key, &refusal, None, now_ms)
                .map_err(|error| error.to_string())?;
            return Ok(refusal);
        }
        Err(error) => return Err(error.to_string()),
    };
    let accepted = outcome(Status::Accepted, None);
    store
        .record_remote_thread(&thread, receipt.thread_id.as_str())
        .and_then(|()| {
            store.record_inbound(
                &key,
                &accepted,
                Some((receipt.message_id.as_str(), &endpoint.key, space.as_str())),
                now_ms,
            )
        })
        .map_err(|error| error.to_string())?;
    wake(&space);
    Ok(accepted)
}

/// The space the item goes to, or the status and reason it is not
/// accepted with. A reply to a message B relayed goes to that message's
/// space. Any other item goes to the one activated space of this server
/// where `item.to` holds a live claim.
fn place(
    main: &mut StateStore,
    store: &RelayStore,
    endpoint: &Endpoint,
    request: &Request,
    item: &Item,
) -> Result<Result<crate::SpaceId, (Status, String)>, String> {
    let here = super::machine_name();
    let unknown = || (Status::Refused, format!("no @{} on {here}", item.to));
    let candidates = match &item.in_reply_to {
        Some(message) => {
            let placed = main
                .transaction(|transaction| {
                    transaction.message_place(&endpoint.key, &MessageId::new(message))
                })
                .map_err(|error| error.to_string())?;
            let Some((space, thread)) = placed else {
                return Ok(Err((
                    Status::Refused,
                    format!("{here} relayed no message {message}"),
                )));
            };
            let mapped = store
                .maps_thread(
                    &request.from.host_id,
                    &endpoint.key,
                    space.as_str(),
                    thread.as_str(),
                )
                .map_err(|error| error.to_string())?;
            if !mapped {
                return Ok(Err((
                    Status::Refused,
                    format!("{here} relayed no message {message}"),
                )));
            }
            vec![space]
        }
        None => main
            .transaction(|transaction| transaction.activated_spaces(&endpoint.key))
            .map_err(|error| error.to_string())?,
    };
    let mut holding = Vec::new();
    for space in candidates {
        let namespace = StateNamespace::for_space(&endpoint.key, space.as_str());
        let (holds, paused) = main
            .transaction(|transaction| {
                Ok((
                    transaction
                        .resolve_routing_alias(&namespace, &item.to)
                        .is_ok(),
                    transaction
                        .worktree_state(&namespace)?
                        .is_some_and(|state| state.paused),
                ))
            })
            .map_err(|error| error.to_string())?;
        if holds {
            holding.push((space, paused));
        }
    }
    match holding.as_slice() {
        [] => Ok(Err(unknown())),
        [(_, true)] => Ok(Err((
            Status::Refused,
            format!("@{} is in a paused space on {here}", item.to),
        ))),
        [(space, false)] => Ok(Ok(space.clone())),
        several => Ok(Err((
            Status::Ambiguous,
            format!(
                "@{} is in more than one session on {here}: {}",
                item.to,
                several
                    .iter()
                    .map(|(space, _)| space.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))),
    }
}

#[cfg(test)]
mod tests;
