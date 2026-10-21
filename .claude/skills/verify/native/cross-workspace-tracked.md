# Tracked handoffs across workspaces

A lead in one Herdr workspace begins a coordination and assigns it to a named agent in another workspace. The coordination, its assignments and their links stay in the lead's workspace. The worker receives a tracked request, replies with a work outcome, and the lead reads, resolves and receives its report as it would for a local worker.

## Sub-features

- `xwst-assign` records one assignment at revision 1 for `#beta-space @reviewer`, forwards the request into Beta Space, and writes coordination rows only in alpha.
- `xwst-envelope` gives the reviewer's envelope alpha's `coordination_id` and assignment, and a bound `reply` whose `work_outcome` defaults to `needs_continuation`.
- `xwst-reply` records the reviewer's `completed` reply against its assignment and delivers it to alpha's lead as a tracked reply.
- `xwst-results` shows the assignment `completed`, its `workspace` `beta-space`, and its delivery `settled` in alpha's `handoff results`.
- `xwst-resolve` nudges the idle lead, resolves the coordination `completed` through the nudge's bound `handoff.resolve`, and delivers the saved report to alpha's lead as `report_to_human`.
- `xwst-no-mirror` leaves no coordination row in Beta Space or gamma, records the reply disposition in Beta Space, and reflects it into alpha.

## How to get to it (user POV)

- Open two Herdr workspaces, activate Namedrop in each, and name an agent in each.
- In the lead's workspace, run `namedrop handoff begin`, then `namedrop send` with its `coordination_id` and a source that starts `#<label> @<name>`.
- The worker answers with the bound `reply`, setting `work_outcome`. The lead reads `namedrop handoff results` and resolves with `namedrop handoff resolve`.

## Driving it with control-namedrop

Preconditions: the same as [cross-workspace mentions](cross-workspace.md), whose setup and cast this recipe shares.

- Run `.claude/skills/verify/native/control-namedrop.py --feature cross-workspace-tracked --binary <candidate>`.
- t-send. Alpha's lead runs `handoff begin`, then `send` `#beta-space @reviewer Review the release notes and reply.` with the `coordination_id`, through the CLI with its pane context. The receipt must carry one link at revision 1 and one forwarded copy in `beta-space`. Only Beta Space's reviewer receives an envelope. Its `control` must carry the coordination and the assignment, and its bound `reply` must default `work_outcome` to `needs_continuation`. `coordinations`, `coordination_assignments` and `coordination_links` must hold rows only for alpha.
- t-reply. The helper runs the reviewer's bound `reply` with `work_outcome` `completed`. Its receipt must name the assignment. Only alpha's lead receives the reply, with the coordination id.
- t-results. Alpha's `handoff results` for the coordination must show the assignment `completed`, `workspace` `beta-space`, and delivery `settled`. The helper completes the reply through its bound `complete.no-reply`.
- t-nudge. Only alpha's lead receives a `coordination_nudge` for the coordination.
- t-resolve. The helper runs the nudge's bound `handoff.resolve` with `outcome` `completed`, then its `complete.no-reply`. Only alpha's lead receives the saved report, as `report_to_human`. No coordination row exists outside alpha. The request's reply disposition exists once in Beta Space and once, reflected, in alpha.
- Every delivery check uses the cross-workspace rule: wait for the expected envelope counts, wait 3 s more, and require every simulated pane's count to match.
- `cross-workspace-tracked.json` keeps every receipt, the results, the row counts per workspace, the dispositions, and every requirement with its observed value. It is saved on every stop, with `<role>-envelopes.json`, `state.sqlite3`, `summary.json` and `cleanup.json`.

## Limits

- The simulated agents never read their envelopes. The helper runs each bound operation for them.
- No lead restart, adoption, remote nudge suppression or missing-disposition warning is driven natively. `tests/cross_workspace_tracking.rs` covers those against FakeHerdr.
