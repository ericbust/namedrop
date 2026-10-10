# Collaboration controls and Doctor

Users inspect blocked collaboration and pause or resume workspace delivery without cancelling work already running.

## Sub-features

- `controls-doctor` reports health without mutating saved state.
- `controls-pause` persists a workspace pause.
- `controls-resume` clears the pause.
- `controls-delivery` holds pending delivery during pause and drains it after resume.
- `controls-targeted` disables one address or pauses/stops/continues one thread.
- `controls-status` shows current work and outstanding requests in Agent status.

## How to get to it (user POV)

- Run `namedrop doctor`, optionally with `--json` or `--human`.
- Press Prefix, release, Shift-H, or run `herdr plugin action invoke doctor --plugin namedrop`.
- Run `namedrop space pause` and `namedrop space resume`.
- Use `namedrop address disable`, `address enable`, or `thread pause`, `thread resume`, `thread stop`, and `thread continue` with structured IDs from the affected name/request.
- Press Prefix, release, Shift-S, or run `herdr plugin action invoke agent-status --plugin namedrop`.

## Driving it with control-namedrop

Preconditions:

- The helper creates an owned activated workspace. Its Doctor runs before every feature.
- Delivery and Agent status checks additionally need active requests and registered sessions in an owned instance.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature controls`.
- Doctor runs `doctor --json`, records its diagnosis, and requires unchanged database contents. The unnamed model-free fixture reports `needs_attention` and `ND-SETUP-003`. This is an expected prerequisite gap, not healthy agent delivery.
- Pause runs `space pause --input-json '{}'`. Read-only SQLite inspection must find the single fixture workspace's `paused` value equal to `1`.
- Resume runs `space resume --input-json '{}'`. A second inspection must find `0`. `controls.json`, CLI transcripts, and `state.sqlite3` retain the result.
- The existing `tests/coordination_native.py` intends to check pause/resume while tracked work is pending, but currently fails earlier on inherited pane provenance. See [handoffs](handoffs.md); do not count it as delivery proof until that fixture is repaired and rerun.
- In a separately linked disposable Doctor popup, use `right`/`left` or `>`/`<` to page, `r` to refresh, then Esc to close. Capture diagnosis before and after; compare stored state to prove refresh is read-only.
- In an owned Agent status popup with active work, use `r` to refresh, up/down to scroll, and Esc to close. Observe the outstanding request before its reply, then its removal after settlement. Provider-backed descriptions need a separately authorized authenticated run.

## Gotchas

- Doctor exits `0` for healthy, `1` for attention, and `2` for unavailable. Only its reported prerequisite gaps determine which drives remain usable.
- Pause/cancel does not kill a running terminal command. Prove pending delivery separately from already running work.
- Doctor and Agent status popups are separate entry points; the helper's CLI check does not cover them.
- Do not resend held or uncertain work. Read its attempt ID and cause before recovery.
- Thread stop requires review of the interruption scope; never exercise it against the user's sessions.
