# Stuck wake

A lead agent receives a coordination nudge, resolves the tracked handoff inside that turn, and ends the turn idle without running the nudge's `complete --no-reply`. Namedrop settles the wake as a no-reply once the acknowledgement grace has passed and dispatches the saved report. Nothing stays queued, and Doctor stays clean.

## Sub-features

- `wake-grace-settle` settles an acknowledged nudge attempt as `no_reply` once 30 s have passed since its lease, with no further event.
- `wake-report-dispatch` dispatches the `report_to_human` delivery that the resolve queued behind the nudge.
- `wake-doctor-clean` leaves no `ND-DELIVERY-003` and no `ND-COORDINATION-001` for that coordination.

## How to get to it (user POV)

- Lead a tracked handoff from a registered agent session. Once every worker has replied and the lead is idle, the lead receives a nudge envelope with `control.coordination_nudge` true.
- In that turn, run the nudge's bound `handoff resolve` with outcome `completed` and a report, then end the turn without running the bound `complete --no-reply`.
- The Namestack verify capture `restart-adopt-stall` reached this after a Herdr restart. The sub-lead received the nudge, resolved in the same turn, and ended the turn with the wake still acknowledged.
- Before the fix, `namedrop handoff results --input-json '{"coordination_id": "<id>"}'` kept showing `report_saved` true and `report_delivery_settled` false, Doctor showed `ND-COORDINATION-001` for that coordination, and the report never reached the pane until the next prompt. That prompt drained the wake without a disposition and left `ND-DELIVERY-003` behind.

## Driving it with control-namedrop

Preconditions:

- Native Herdr with `herdr pane report-agent-session --agent-session-id`, `rustc` on PATH to compile the simulator, `python3` for `setup.sh`, Bash, and `codex` when present. With `codex` on PATH, activation installs the Namedrop Codex plugin into the fixture's `CODEX_HOME`. Without it, activation records that Codex is not set up and continues.
- The helper links the candidate as a disposable plugin with `herdr plugin link` and activates it with `scripts/setup.sh activate --json`, so Herdr's real `pane.agent_status_changed` hook drives the status worker. Namedrop state lives in Herdr's plugin state directory under the fixture.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature stuck-wake --binary <candidate>`.
- The helper starts a compiled stdin-to-stdout program named `codex` in the worker pane and in a third pane for the lead. A renderer draws the Codex idle or working screen from a surface file. The helper reports a fixed session id per pane and waits until `herdr pane list` shows each pane settled with that session.
- `name lead --type agent` and `name worker --type agent` run through the installed launcher with explicit `--herdr-pane`, because the simulator owns each agent pane's input. One working-then-idle flip per pane lets onboarding register both sessions automatically. The helper waits for two active `integration_registrations` rows and then requires Doctor's `available_agents` to be 2.
- The lead begins a tracked handoff and sends `@worker Compute the synthetic checksum and reply with it.` The worker answers through its envelope's bound `reply` with `work_outcome` `completed`. The lead completes the reply envelope, and the nudge arrives. The nudge attempt's `created_at_ms` is T0.
- The lead renders working, runs the nudge's bound `handoff.resolve` with outcome `completed` and a one-line report, and renders idle. The helper requires Herdr's idle observation within 25 s of T0. A later observation stops the run as inconclusive instead of failing it. `summary.json` records the result in `outcome`, which is `inconclusive` for that stop, `stalled` for a stall-window timeout and `passed` after the Doctor assertions. A run that fails before the nudge turn has no `outcome`.
- The stall window polls every 0.5 s for up to 50 s. It reads only the wake attempt's state and disposition, the report delivery's state and the lead's envelope file. No status change and no Namedrop operation happen in this window. A timeout runs Doctor from the operator pane, backs up the database to `state.sqlite3`, saves `stuck-wake.json` with `stalled` true, and fails with `stuck wake: attempt <id> still acknowledged with disposition none <n> s after idle; report delivery queued; no report envelope reached the lead`.
- The helper measures the quiet window. It records the start of the idle hook row that ended the nudge turn. After arrival, or at the stall, it lists every plugin log row that started strictly between that hook and the arrival or stall time as `events_in_window`. A pass requires that list to be empty, so only the worker that the idle hook started can have settled the wake.
- On arrival the wake attempt must be `settled` with disposition `no_reply`. The helper runs Doctor from the operator pane before completing the report and requires no `ND-DELIVERY-003`, because the wake already carries its disposition. That read still shows `ND-COORDINATION-001` for this coordination, since the report delivery has not settled yet. It then completes the report through its bound `complete.no-reply` and runs Doctor again. No issue may carry `ND-DELIVERY-003`, and no `ND-COORDINATION-001` may name this coordination in `diagnostic.source_id`.
- `stuck-wake.json` keeps panes, sessions, registrations, the coordination and wake attempt ids, T0, the idle observation and its hook start, the first poll that saw the settle, the arrival or stall time, every poll, the final wake row and disposition, the report delivery state and `events_in_window`, on both the passing and the stalled path. A pass adds the two Doctor reads as `doctor_after_settle` and `doctor_after_report`. A stall adds `doctor_at_stall`. `lead-envelopes.json` and `worker-envelopes.json` keep every envelope each simulator received. A passing run on this host settled at about T0 + 30.1 s and received the report about 0.5 s later. Cleanup counts the simulator and renderer PIDs with the shells, then scans for any process that still references the fixture and terminates it after a 15 s wait.

## Gotchas

- The grace is 30 s from the wake attempt's `created_at_ms`, the lease time, not from the idle observation. The 25 s idle budget keeps the turn inside it. A slow host that misses the budget gets an inconclusive stop, not a failure.
- Automatic registration needs `name --type agent` on a pane whose detected agent session is live, and then a settled status event. Without the plugin link no hook fires, so nothing registers and no drain runs.
- Herdr reports the first prompt as `idle` and a prompt after work as `done`. Both are settled, and the helper accepts either when the surface is idle.
- The simulator echoes an envelope before the dispatcher commits the attempt out of `dispatching`. An operation recorded from `dispatching` is refused, so every bound operation first waits for its attempt to be `submitted`, `acknowledged` or `settled`.
- The simulated agent never reads its envelopes and runs no model. The helper runs the bound operations on its behalf, so this proves the product's event handling and transport, not agent behavior or real agent hooks.
