# Saved mention groups

Users save several named agents under one alias, then address that group in later requests. Editing a group affects future requests.

## Sub-features

- `groups-save` creates a group with existing agent aliases.
- `groups-list` reports the group and exact members.
- `groups-remove` removes the group without removing its agents.
- `groups-send` expands mentions once per recipient, leaves out the sender, and retains an existing request's recipients after edits.

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
- It removes `review-team` and requires `removed: true`. A second group list must return an empty `data.groups`, and `address list` must still contain `reviewer` and `tester`.
- It requires `group set` with no members to be rejected. It then names the worker pane `@build` as a terminal. That pane already holds `@tester`, so this renames it and changes its type. The recipe records whether a group `ops` containing only `build` is accepted, in `group-probes.json`. Today it is accepted; see Gotchas.
- It saves a group `solo` whose only member is `reviewer`, the lead pane's own name, then sends `@solo Note to self.` from that pane. The send must be refused with `addressed only to its author` and leave the database unchanged. `group-probes.json` keeps the refusal.
- To drive the group prompt entry point in an authorized disposable agent session, enter `@review-team Review this fixture change.` from a session that is not a member. Observe each recipient envelope and its final reply. A member that sends to its own group gets no envelope of its own. Edit the group, then repeat the send with the same `request_id`. The replay must return the original message ID and recipient IDs. This path is not exercised by the helper.

## Gotchas

- `group list` returns an object containing `groups`.
- Groups need 1–64 existing members and cannot contain groups. Members are checked against saved names of any type, so a terminal name is accepted although the error text says "agent members". `group set` also requires an activated space.
- Group and agent aliases exclude each other in both directions. A group cannot claim an agent's alias, and an agent cannot be named or renamed to a group's alias.
- Removing a group that does not exist succeeds with `removed: false`. A member whose name was retired lists with a null `alias`.
- Overlapping group/agent mentions must deliver once per agent. A list check alone does not prove routing.
- Expansion drops the sender, so a message never reaches its own author. A group mention that leaves only the sender is refused with `this message is addressed only to its author; mention at least one other agent`. Preview applies the same rule.
- Saved shell-pane agent names do not establish delivery readiness.
