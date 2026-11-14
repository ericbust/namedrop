//! The `namedrop.relay` action's request, as A's `rex` sends it to B's
//! bridge through `client.perform`.

use serde::Serialize;

/// The `rex` CLI refuses a request over 64 KiB before it sends it, so an
/// item whose request passes this is refused before it is queued.
pub(super) const REQUEST_LIMIT: usize = 60 * 1024;

/// The longest id Rex or B accepts for each field that is not known until
/// the item is sent.
const LONGEST_ID: &str = "................................................................................................................................";

#[derive(Serialize)]
pub(super) struct Perform<'a> {
    pub client_id: &'a str,
    pub action: &'static str,
    pub args: Args<'a>,
}

#[derive(Serialize)]
pub(super) struct Args<'a> {
    pub op: &'static str,
    pub expect_host_id: &'a str,
    pub from: Sender<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Item<'a>>,
    pub ack_through: i64,
}

#[derive(Serialize)]
pub(super) struct Sender<'a> {
    pub host_id: &'a str,
    pub name: &'a str,
    pub host: &'a str,
}

#[derive(Serialize)]
pub(super) struct Item<'a> {
    pub id: &'a str,
    pub to: &'a str,
    pub thread: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub text: &'a str,
}

/// The encoded size of the `deliver` request for one item, with every id
/// not yet known at its longest.
pub(super) fn deliver_size(sender: &str, to: &str, text: &str) -> usize {
    let host = &LONGEST_ID[..64];
    serde_json::to_vec(&Perform {
        client_id: LONGEST_ID,
        action: "namedrop.relay",
        args: Args {
            op: "deliver",
            expect_host_id: LONGEST_ID,
            from: Sender {
                host_id: LONGEST_ID,
                name: sender,
                host,
            },
            item: Some(Item {
                id: LONGEST_ID,
                to,
                thread: LONGEST_ID,
                in_reply_to: Some(LONGEST_ID),
                text,
            }),
            ack_through: i64::MAX,
        },
    })
    .map_or(usize::MAX, |encoded| encoded.len())
}
