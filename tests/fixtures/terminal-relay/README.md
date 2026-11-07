These fixtures were recorded in the isolated `test-terminal-relay` testbed on 2026-11-04 with Claude Code 2.1.316 and Codex 0.164.0. Stop inputs preserve the recorded fields and assistant text. Session identifiers and filesystem paths were replaced with test values.

Transcript fixtures contain only the assistant text records and completion event needed to check the fallback parser. Claude's earlier and final answers show that the latest assistant message wins. Codex records retain the native turn metadata used to match the Stop payload. They contain no credentials or production transcripts.

Both hosts supply `last_assistant_message` directly on `hook_event_name: Stop`. The transcript reader is a fallback only when that field is absent or null. An explicitly empty field remains empty.
