# Shared checkout

Users open a second Herdr workspace on a checkout that another workspace already has open, and Namedrop keeps working in both. Each workspace keeps its own names. Both report the same checkout as their worktree.

## Sub-features

- `shared-doctor` reports health in the second workspace instead of `ND-INSPECT-001`.
- `shared-activate` activates the second workspace on the same checkout path under its own space ID.
- `shared-names` keeps names saved in one workspace out of the other workspace's roster.
- `shared-unscoped` keeps a caller that names no workspace out of the wrong workspace. With `--herdr-pane` alone it reads the pane's workspace. With only the checkout as cwd it refuses, because two workspaces own that checkout.

## How to get to it (user POV)

- With Namedrop active in a workspace, open a second workspace on the same directory, for example with `herdr workspace create --cwd <that checkout>`.
- In the new workspace, run `namedrop doctor`, `namedrop activate`, and `namedrop name @alias --type terminal` as usual.
- Run `herdr worktree list --workspace <new workspace> --json` to see the shape Namedrop has to handle. The checkout appears once, with the first workspace as `open_workspace_id`. The `source` object names the new workspace as `source_workspace_id` because the new workspace sits on the repository root.

## Driving it with control-namedrop

Preconditions:

- The helper creates the activated primary workspace and runs its Doctor first.
- The recipe creates a twin workspace on the same fixture directory and waits for its root pane's shell prompt before any command runs there.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature shared-checkout`.
- The helper captures `herdr worktree list --workspace <twin> --json`. The listing must name the primary workspace as the checkout's opener and the twin as `source.source_workspace_id`.
- Doctor runs `doctor --json` in the twin pane with the twin's workspace. Its status must be `healthy` or `needs_attention`, and no issue may carry `ND-INSPECT-001`.
- Activation runs `activate --input-json '{"confirmed": true}'` in the twin. The response's `space_id` must be the twin, and its `worktree_id` must equal the fixture workspace path. The primary activation returned the same `worktree_id` under its own `space_id`.
- Naming runs `name @twin-reviewer --type terminal` in the twin and `name @verify-build --type terminal` in the primary pane. `address list --json` in each workspace must return disjoint rosters, `verify-build` in the primary and `twin-reviewer` in the twin.
- Doctor runs once more in each workspace. Neither report may carry `ND-INSPECT-001`.
- Two `address list --json` calls run from the fixture checkout outside any pane, with no `--herdr-workspace` and no `HERDR_WORKSPACE_ID`. The one with `--herdr-pane <twin root pane>` must return the twin's roster. The one with no pane at all must exit 65 with an error that names both workspace ids and `--herdr-workspace`. The helper saves `herdr workspace list` and `herdr worktree list --cwd <checkout> --json` beside them.
- `shared-checkout.json` retains the worktree listing, both activation results, both rosters, both Doctor reports, the workspace list, the by-cwd worktree listing, and both unscoped calls. Teardown verifies that the twin's shell exited with the others.

## Gotchas

- Herdr lists each checkout path once, with the first opener as `open_workspace_id`. Namedrop attaches the second workspace through `source` only when `source.source_workspace_id` names the requester, which happens when the requester sits on the repository root.
- A second workspace on a linked worktree that another workspace already has open still fails closed with `ND-INSPECT-001`. For that requester, `source` names whichever workspace holds the repository root, so Namedrop has no proof of the requester's checkout. This boundary is deliberate, and the helper does not drive it.
- Both workspaces share one `worktree_id`. It identifies the checkout for checkout proofs, not the workspace. Saved state is keyed by workspace. The helper proves that separation through names only.
- `herdr workspace get <id>` can report a null `worktree.checkout_path` for a workspace created with `--cwd`. Do not use it as the checkout proof.
- `herdr worktree list --cwd <checkout>` names only the first opener, in `open_workspace_id` and in `source`. The second workspace shows up only in `herdr worktree list --workspace <second>`, through `source`. A caller that names neither a workspace nor a pane cannot be placed from cwd alone when two workspaces own the checkout.
