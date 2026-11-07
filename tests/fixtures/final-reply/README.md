These fixtures were recorded on 2026-11-07 with Claude Code 2.1.319 and Codex 0.167.0, each in an isolated home with UserPromptSubmit and Stop hooks that saved their input. Session, prompt, turn, task and message identifiers and filesystem paths were replaced with test values; prompts and assistant text are as recorded.

- Both hooks of one turn carry the same turn id: Claude Code's `prompt_id`, Codex's `turn_id`.
- A Claude Code Stop with a background shell still running lists it in `background_tasks` with status `running`. The completion arrives as a new prompt that starts with `<task-notification>`, with its own `prompt_id`.
- An interrupted turn gets no Stop on either host. Claude Code writes a `[Request interrupted by user]` user record for that prompt; Codex writes an `event_msg` of type `turn_aborted` for that turn.
