# Direct messages

The operator gets one private conversation per agent address in its home workspace. Workspace identity and pane address identify the peer. An alias is display text, so renaming an agent preserves its conversation and duplicate aliases never share history.

## Store representation

No database migration is needed. Store a canonical thread in the recipient's existing namespace with a deterministic thread ID derived from its stable address. The immutable operator-authored root uses the existing unrestricted `grammar_version` field with `direct-message/v1`. Its single immutable recipient identifies the peer. Existing message and recipient triggers prohibit updates. The store resolves a typed DM identity from the root and validates it on every append. A thread prefix alone is insufficient evidence of a DM.

Only the reserved local operator and that root recipient can author messages in the conversation. Delivery always selects the other participant by address. Mentions in the body are literal content and never add recipients. Store and reply validation enforce this even when a caller bypasses Channels. DMs use existing delivery and mailbox machinery in the peer's home workspace without envoy copies. Agent replies neither check nor decrement the Automatic Reply Limit.

## Directory and closed peers

The server combines its existing workspace discovery with store address records across every visible workspace. Each entry carries its space key, stable address, alias, channel label, runtime, presence, and pane label. DM history retains retired addresses and alias snapshots beyond ordinary recent-member retention. Closed entries remain readable and reject sends. The sidebar sorts by latest DM activity, then workspace, supports filtering, and groups closed entries separately.

## Privacy of placement

Store reads exclude complete DM threads before channel snapshot and history pagination. Channel thread lists, message body and context endpoints, mailbox and attention rows, router context, and All activity use the same classification. Dedicated DM reads select only the canonical peer conversation. DM unread and attention state belong to the DM entry. The browser never reconstructs a DM from ordinary channel messages.

## Delivery envelope

An explicit trusted DM mode identifies the message as coming from the operator and binds its reply operation to this conversation and operator. Its instruction states that body mentions cannot add recipients and that the reply belongs in the DM. A reply retains the DM classification by its canonical thread, even when its own message grammar differs. The server validates the author and recipient against that identity before storing or delivering it.

## Design checkpoint

Inspection confirmed that `collaboration_messages.grammar_version` is unrestricted text, `create_thread` accepts an explicit ID, and recipient rows preserve address identity. This design changes code and wire types while retaining database schema 6. Implementation can proceed.
