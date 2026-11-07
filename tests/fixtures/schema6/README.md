# Schema 6 migration fixtures

Run `cargo test --test schema6_migration` to check both populated histories.
For a JSON report, run `python3 tests/schema6_migration.py --binary target/debug/namedrop --evidence /tmp/schema6-migration.json`.

`schema.sql` freezes the schema recorded in `src/store/schema-v6.txt` at experimental 1.5.0-exp.13 (`5ce71ac`) and main 1.4.4 (`b28ef08`).
Those snapshots are identical. The fixture orders tables before indexes and keeps schema version 6.
It does not derive its schema from the current migration code.

`common.sql` supplies existing messages, a held delivery, a paused conversation, a persona, a group, and an unfinished coordination.
`experimental.sql` adds a private DM, a terminal relay and its deduplication facts, bridge mappings, and a shared terminal with a completed execution.
These are synthetic records in the historical persistence formats, rather than a copy of anyone's state.
DMs use `collaboration_messages` and `message_recipients`. Relay watermarks use `transition_facts`.
Subagent lifecycle facts use hashed paths under `subagents/v1`, outside SQLite.
The Python fixture writes that file format. The Rust test also records and reads a child through the public subagent API.

The CLI check holds an isolated runtime command gate and calls `--migrate-runtime-state`, which opens `StateStore`.
It compares every original column value and every subagent file byte after migration and after a second open.
It also checks foreign keys, SQLite integrity, the delivery-seats view, and empty reminder tables.
An additional fixture lacks the remote-terminal column and delivery-seats view, as an earlier experimental schema 6 store can.
The Rust check keeps one migrated store handle open while opening another, then closes both and reopens the database.
