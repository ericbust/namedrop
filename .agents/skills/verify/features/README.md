# Namedrop verification map

This map records user entry points and observable proof. Read it before choosing a drive. A CLI check does not cover the corresponding agent prompt, shortcut, or floating popup.

## Baseline preconditions

- Run `cargo build --locked --bin namedrop` from the repository root.
- Have native Herdr, Bash, Git, and Python 3.9+ on PATH.
- Use `scripts/control-namedrop.py` through its repository path, `.agents/skills/verify/scripts/control-namedrop.py`.
- Let each invocation create its own home, socket, workspace, panes, and Namedrop state. Its Doctor runs before feature actions.
- No model credentials are copied into the fixture. Forced agent names in plain shell panes prove saved names and personas, not connected agent sessions.

## Driving conventions

The helper is named `control-namedrop` in these recipes. It runs each CLI action in a real Herdr pane and records the exact command with explicit context. Saved name identifiers come from `address list`'s `id` field; mutation inputs use `address_id`. Group list returns `data.groups`, not a bare array.

Each invocation starts from a fresh activated workspace with no names. Use the matching feature switch. To expand a drive, keep its public commands and assertions together in the helper so a later agent can repeat it. Never patch SQLite to arrange a result. Read-only database inspection is evidence.

## Proof and skip reporting

Record the feature ID, user entry point, tested commit and binary hash, actions, output, persisted effects, and cleanup. The helper's evidence stays under `.scratch/verify/`; native collaboration uses its own retained directory. Evidence is local and ignored by Git.

State which paths were exercised and which remain unverified. A successful model-free check cannot prove authenticated hooks or collaborator choice. For blocked paths, record the unmet prerequisite and attempted command. Do not send requests to agents in the user's active workspace to complete a fixture check.

## Features

- [Names and personas](names.md) covers terminal naming, rename, persona preservation/clearing, and naming/management popups.
- [Groups](groups.md) covers creation, list, membership, removal, and group mentions.
- [Collaboration controls and Doctor](controls.md) covers workspace pause/resume and health, with address/thread controls as separate drives.
- [Shared checkout](shared-checkout.md) covers a second workspace opened on a checkout that another workspace already has open, with Doctor, activation, and separate names in each workspace.
- [Tour](tour.md) covers page navigation, close, reset, automatic opening, and popup entry points.
- [Handoffs, terminal work, and context](handoffs.md) covers direct/delegated mentions, preview, replies, tracked reports, shell results, and freshness.
- [Stuck wake](stuck-wake.md) covers a coordination nudge whose turn ends idle inside the acknowledgement grace without a completion, its settle as no-reply, the report dispatch, and Doctor, through real Herdr status hooks against simulated Codex sessions.

Not yet mapped: the `deactivate` and `update` plugin actions, the startup reconcile, and pane lifecycle events. The helper uses activation as a precondition. The stuck-wake recipe is the one drive that links the candidate as a plugin. It exercises the `activate` plugin action through `setup.sh` and the `pane.agent_status_changed` hook. The shared-checkout recipe also checks its space and worktree IDs.
