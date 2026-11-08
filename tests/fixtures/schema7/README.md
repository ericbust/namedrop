# Schema 7 upgrade fixture

Run `cargo test --test schema7_upgrade`.

`state.sql` is a `.dump` of a real Namedrop 1.6.0 state database, taken from an isolated testbed profile after a passing real-agent run and a few Channels posts. Its last direct message, to the Claude pane `w1:p2`, is a delivery left open: Claude ended the turn with background work running, so a claim held it. It holds no personal data; the messages are test words.

`typed-envelope.txt` is that delivery's prompt exactly as 1.6.0 typed it, read from Claude's transcript, including Claude Code's paste markers. The profile's state directory is replaced by `/STATE`. The test puts the real paths back, because authentication compares every byte.

`golden/` holds what 1.6.0 typed for other kinds of delivery, each open at the moment it was captured: an agent's ask, a person's Mention, the reply to a Mention, a result set, a tracked assignment, a coordination nudge and a channel post with a recap. `generate_goldens.rs` wrote all but the last against caf657d, and its first lines say how to run it. `channel-recap` comes from a real Channels post to real Claude on a caf657d testbed profile, held open by a background task, with its `transition_facts` rows left out to save space. `schema.sql` is shared and each `<kind>.sql` holds that moment's rows, with `/STATE` and `/EXE` for the paths. Do not regenerate these from newer code: they exist to fail when an envelope's bytes change, because a prompt typed before an upgrade authenticates by exact equality.
