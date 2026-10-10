# Saved mention groups

Users save several named agents under one alias, then address that group in later requests. Editing a group affects future requests.

## Sub-features

- `groups-save` creates a group with existing agent aliases.
- `groups-list` reports the group and exact members.
- `groups-remove` removes the group without removing its agents.
- `groups-send` expands mentions once per recipient and retains an existing request's recipients after edits.

## How to get to it (user POV)

- Ask an agent to create `@review-team` containing `@reviewer` and `@tester`.
- Run `namedrop group set --input-json '{"alias":"review-team","members":["reviewer","tester"]}'`.
- Run `namedrop group list --json` or `namedrop group remove --input-json '{"alias":"review-team"}'`.
- Start a prompt with `@review-team` to hand it off, or ask the current agent to delegate to the group.

## Driving it with control-namedrop

Preconditions:

- The helper owns an activated workspace with two shell panes.
- Group send additionally needs real registered agents or the native transport fixture described in [handoffs](handoffs.md).

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature groups`.
- The recipe saves `@reviewer` and `@tester` with agent type, then runs the exact `group set` command above.
- It runs `group list --json` and asserts one `review-team` group whose member aliases are exactly `reviewer` and `tester`. Inspect `groups.json` and `commands.jsonl`.
- It removes `review-team`, then reads the list again and requires `data.groups` to be empty. The final database snapshot retains both names.
- To drive the group prompt entry point in an authorized disposable agent session, enter `@review-team Review this fixture change.`. Observe each recipient envelope and its final reply. Edit the group after the request starts and prove the original request keeps its original recipient IDs. This path is not exercised by the helper.

## Gotchas

- `group list` returns an object containing `groups`.
- Groups need 1–64 existing agent members, cannot contain groups, and cannot claim a reserved agent alias.
- Overlapping group/agent mentions must deliver once per agent. A list check alone does not prove routing.
- Saved shell-pane agent names do not establish delivery readiness.
