# Schema 7 upgrade fixture

Run `cargo test --test schema7_upgrade`.

`state.sql` is a `.dump` of a real Namedrop 1.6.0 state database, taken from an isolated testbed profile after a passing real-agent run and a few Channels posts. Its last direct message, to the Claude pane `w1:p2`, is a delivery left open: Claude ended the turn with background work running, so a claim held it. It holds no personal data; the messages are test words.

`typed-envelope.txt` is that delivery's prompt exactly as 1.6.0 typed it, read from Claude's transcript, including Claude Code's paste markers. The profile's state directory is replaced by `/STATE`. The test puts the real paths back, because authentication compares every byte.
