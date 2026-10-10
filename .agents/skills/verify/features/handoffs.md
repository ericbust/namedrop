# Handoffs, terminal work, and context

Users mention collaborators, receive replies where they asked, track partial and final results, run commands in named terminals, and inspect whether saved reviews still match the reviewed checkout.

## Sub-features

- `handoff-direct` routes a leading mention and returns a reply to its requester.
- `handoff-delegated` leaves a non-leading mention with the current agent to delegate.
- `handoff-preview` expands recipients and proposed context without sending or capturing state.
- `handoff-tracked` retains progress, continuation, and one final report.
- `handoff-terminal` returns shell stdout, stderr, and exit status.
- `handoff-freshness` distinguishes fresh, stale, and unknown against pinned evidence.

## How to get to it (user POV)

- Start a prompt with `@reviewer Check this fixture change.` for a direct handoff.
- Enter `Ask @reviewer to check this fixture change and use the result.` to delegate while retaining the current goal.
- Start with `Preview sending this to @reviewer: check this fixture change.` or run `namedrop handoff preview --input-json '{"source":"@reviewer Check this fixture change."}'`.
- Ask what is still outstanding, or run `namedrop handoff status` and `handoff results` with the request/coordination IDs returned by the send.
- Name a shell with `namedrop name @build --type terminal`, then send `@build printf 'fixture result\n'` from an agent.
- Ask for a review with pinned Git context and explicit diff evidence, then read its result after changing a reviewed file.

## Driving it with control-namedrop

Preconditions:

- Build `target/debug/namedrop` and have native Herdr, Rust, Python, and Bash or zsh available.
- Real prompt consumption needs two registered authenticated host sessions in an owned workspace. Saved names alone are insufficient.

- The existing native scripts are currently blocked. `python3 tests/coordination_native.py --binary target/debug/namedrop` fails at `name lead --type agent`, and `NAMEDROP_TEST_BINARY="$PWD/target/debug/namedrop" python3 tests/personas_native.py` fails at activation. Both invoke the binary from Python with an inherited `HERDR_PANE_ID`; their process is outside the fixture pane's shell tree. The product returns exit `65` and tells the caller to pass `--herdr-pane`. Failure evidence from skill creation is under `.scratch/verify/legacy-native/`.
- Before using those checks, repair the fixture's public CLI invocations with explicit fixture `--herdr-pane`, `--herdr-socket`, and `--herdr-workspace`, or run them from the owned pane. Hook wrappers need their normal in-pane process ancestry; do not forge it. This skill does not bypass provenance checks.
- The coordination check's intended drive launches isolated Herdr, stages the candidate plugin, uses compiled agent simulators, receives envelopes, executes bound reply/completion tools, reads progress/results, resolves a report, and checks persistence. After repair, require `NAMEDROP_COORDINATION_NATIVE_OK`, successful `cleanup.json`, captured envelopes, `commands.jsonl`, and `state.sqlite3`. Successful evidence lives under `.scratch/leader-continuation/native/namedrop-coordination-*/`. Failure prints its retained `/tmp/namedrop-coordination-*` fixture; preserve evidence before removing scratch state.
- The persona check's intended drive records emitted installed wrappers under `.scratch/personas/native/` on success. It does not start a model. Wrapper output, host consumption, and collaborator choice remain separate checks after the fixture repair.
- For a real direct handoff, submit the exact leading-mention prompt above in an owned authenticated sender pane, capture the recipient request, reply, and requester receipt. Repeat in the other direction. The transport check does not prove either host consumed its hook.
- For delegated mentions, submit the non-leading prompt above and prove the current agent retains the goal, delegates, reads the reply, and completes its own response. Record actual agent behavior separately from simulated envelopes.
- For preview, use the exact CLI command above from a registered sender. Before and after, snapshot the database through a read-only connection, run `git show-ref` and enumerate the fixture's `.git/objects`, and capture recipient transcripts. Require expanded recipients and unchanged messages, deliveries, context grants, refs, objects, and recipient input. Context preview needs evidence paths from a disposable committed fixture. This authenticated path is not run by the default helper.
- For terminal work, send the harmless `printf` request above only to an owned shell. Require the output in that pane and the saved result's stdout/stderr/exit status, then its receipt in the sending agent. A nonzero exit must settle once without automatic rerun.
- For freshness, record the returned message ID and pinned paths, read the result while unchanged, modify a reviewed file, then read again from the same authorized sender. Expect `fresh`, then `stale`. An unrelated file change must not invalidate it; unavailable checkout/evidence must report `unknown` with a reason.

## Gotchas

- Simulated native sessions prove transport and public commands, not collaborator selection or provider-backed reasoning.
- An inherited pane ID can be rejected when the CLI runs outside that pane's process tree. Use the exact bound tool's explicit pane/socket/workspace arguments.
- Preview requires a currently registered sender and does not establish that a later send will still resolve the same recipients.
- Partial-result reads must not consume the final report. A completed sender turn alone does not prove recipient delivery or reply receipt.
- Terminal execution and Git context have real side effects. Use only disposable files and panes, retain before/after evidence, and do not resend an uncertain attempt.
- The stock native checks have their own cleanup behavior. Run their cleanup even after failure and verify the recorded owned handles, never processes selected by name.
