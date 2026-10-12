# Shared checkout

Users open a second Herdr workspace on a checkout that another workspace already has open, and Namedrop keeps working in both. Each workspace keeps its own names. Both report the same checkout as their worktree.

## Sub-features

- `shared-doctor` reports health in the second workspace instead of `ND-INSPECT-001`.
- `shared-activate` activates the second workspace on the same checkout path under its own space ID.
- `shared-names` keeps names saved in one workspace out of the other workspace's roster.
- `shared-unscoped` keeps a caller that names no workspace out of the wrong workspace. With `--herdr-pane` alone it reads the pane's workspace. With only the checkout as cwd it refuses, because two workspaces own that checkout.
- `shared-linked` does the same for a linked worktree. A second workspace on a linked worktree that another workspace opened cannot be placed, so a caller with only that checkout as cwd must be refused rather than handed the first workspace.

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
- The recipe then commits the fixture, adds a linked worktree with `git worktree add`, opens it from the primary workspace with `herdr worktree open`, and creates a second workspace on it with `workspace create --cwd`. Herdr refuses a second `worktree open` of an open path and returns the existing workspace, so `--cwd` is the only way to get the second workspace.
- The first linked workspace activates and names `@linked-build`. Activation in the second linked workspace must fail with `does not contain space <id>`, the `ND-INSPECT-001` boundary below.
- Three `address list --json` calls run outside any pane with no workspace. From the linked checkout with `--herdr-pane <second linked pane>`, it must exit 65 without the first workspace's roster. From the linked checkout with no pane, it must exit 65 naming both linked workspaces and `--herdr-workspace`. From the repository root with no pane, it must exit 65 naming the primary and the twin and neither linked workspace.
- `shared-linked-checkout.json` retains the `worktree open` result, the second linked workspace's worktree listing, the workspace list, the first linked roster, the refused activation, and the three unscoped calls.
- `shared-checkout.json` retains the worktree listing, both activation results, both rosters, both Doctor reports, the workspace list, the by-cwd worktree listing, and both unscoped calls. Teardown verifies that the twin's shell exited with the others.

## Gotchas

- Herdr lists each checkout path once, with the first opener as `open_workspace_id`. Namedrop attaches the second workspace through `source` only when `source.source_workspace_id` names the requester, which happens when the requester sits on the repository root.
- A second workspace on a linked worktree that another workspace already has open still fails closed with `ND-INSPECT-001`. For that requester, `source` names whichever workspace holds the repository root, or no workspace when none does, so Namedrop has no proof of the requester's checkout. This boundary is deliberate. The helper drives it and requires the refusal.
- Both workspaces share one `worktree_id`. It identifies the checkout for checkout proofs, not the workspace. Saved state is keyed by workspace. The helper proves that separation through names only.
- Herdr 0.9.1 fills a workspace's `worktree` object (`checkout_path`, `repo_key`, `is_linked_worktree`) in `workspace list` and `workspace get` only through `herdr worktree open` or `herdr worktree create`. It stamps the workspace those commands open, and the source workspace named by their `--workspace` or `--cwd`. A workspace made by `workspace create --cwd` has no `worktree` key, even on a repository root, and `worktree list` does not add one. When the key is present, `checkout_path` is that workspace's checkout. When it is absent, the checkout is unknown, not null.
- `herdr worktree list --cwd <checkout>` names only the first opener, in `open_workspace_id` and in `source`. The second workspace shows up only in `herdr worktree list --workspace <second>`, through `source`. A caller that names neither a workspace nor a pane cannot be placed from cwd alone when two workspaces own the checkout.
