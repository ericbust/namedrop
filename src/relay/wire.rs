//! The `namedrop.relay` action: the request A's `rex` sends to B's bridge
//! through `client.perform`, and the result B answers with.

use serde::{Deserialize, Serialize};

/// The `rex` CLI refuses a request over 64 KiB before it sends it, so an
/// item whose request passes this is refused before it is queued.
pub(crate) const REQUEST_LIMIT: usize = 60 * 1024;
/// B refuses a longer `text` itself, because a direct client is not held to
/// A's CLI.
pub(crate) const TEXT_LIMIT: usize = 60 * 1024;
/// A result over the server's 1 MiB breaks B's own connection, so B pages
/// its outbox at this.
pub(crate) const PAGE_LIMIT: usize = 256 * 1024;

/// Whether `value` is an id B or A takes from the other side: 1 to 128
/// printable ASCII characters with no space.
pub(crate) fn valid_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// The longest id Rex or B accepts for each field that is not known until
/// the item is sent.
const LONGEST_ID: &str = "................................................................................................................................";

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Perform {
    pub client_id: String,
    pub action: String,
    pub args: Request,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Op {
    Deliver,
    Collect,
}

/// The action's arguments.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Request {
    pub op: Op,
    pub expect_host_id: String,
    pub from: Sender,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Item>,
    #[serde(default)]
    pub ack_through: i64,
    /// The outbox epoch `ack_through` counts in. B drops nothing for an
    /// ack from another epoch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub epoch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Sender {
    /// A's `host_id`, as A's server reported it.
    pub host_id: String,
    /// The sender's alias on A.
    pub name: String,
    /// A's machine name, for display.
    pub host: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Item {
    /// A's message id.
    pub id: String,
    /// A name on B.
    pub to: String,
    /// A's thread id.
    pub thread: String,
    /// The B message it answers, when it answers one.
    pub in_reply_to: Option<String>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Accepted,
    Duplicate,
    Refused,
    Ambiguous,
    Retry,
}

/// What B decided for one item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Outcome {
    pub id: String,
    pub to: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One reply B keeps for A.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Reply {
    pub seq: i64,
    /// The A message it answers.
    pub re: String,
    /// A's thread.
    pub thread: String,
    /// The B alias that wrote it.
    pub from: String,
    /// B's message id.
    pub message_id: String,
    pub text: String,
}

/// The action's result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Answer {
    pub host_id: String,
    pub can_reply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Outcome>,
    #[serde(default)]
    pub outbox: Vec<Reply>,
    #[serde(default)]
    pub more: bool,
    /// The epoch of B's outbox, new each time B's relay database is
    /// created, so A knows when B's `seq` started again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub epoch: String,
}

/// The encoded size of the `deliver` request for one item, with every id
/// not yet known at its longest.
pub(crate) fn deliver_size(sender: &str, to: &str, text: &str) -> usize {
    serde_json::to_vec(&Perform {
        client_id: LONGEST_ID.to_owned(),
        action: "namedrop.relay".to_owned(),
        args: Request {
            op: Op::Deliver,
            expect_host_id: LONGEST_ID.to_owned(),
            from: Sender {
                host_id: LONGEST_ID.to_owned(),
                name: sender.to_owned(),
                host: LONGEST_ID[..64].to_owned(),
            },
            item: Some(Item {
                id: LONGEST_ID.to_owned(),
                to: to.to_owned(),
                thread: LONGEST_ID.to_owned(),
                in_reply_to: Some(LONGEST_ID.to_owned()),
                text: text.to_owned(),
            }),
            ack_through: i64::MAX,
            epoch: LONGEST_ID.to_owned(),
        },
    })
    .map_or(usize::MAX, |encoded| encoded.len())
}
