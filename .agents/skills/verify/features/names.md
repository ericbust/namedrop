# Names and personas

Users name panes so other agents can mention them, manage saved names, and describe an agent's specialization with an optional persona.

## Sub-features

- `names-terminal` saves a shell pane as `@build` with terminal type.
- `names-rename` changes an alias while retaining its saved identity.
- `names-persona` saves a persona, preserves it on repeated naming, and clears it with `--generic`.
- `names-popup` names or cancels from the naming popup.
- `names-management` renames, edits an agent's persona, attaches/detaches, enables/disables, or removes from the manager.

## How to get to it (user POV)

- Run `namedrop name @build --type terminal` or `namedrop name @reviewer --persona 'Reviews Rust changes.'`.
- Ask an agent to name itself and describe its persona.
- Press Prefix, release, Shift-I, or run `herdr plugin action invoke name-agent --plugin namedrop`.
- Press Prefix, release, Shift-M, or run `herdr plugin action invoke manage-addresses --plugin namedrop`.
- Run `namedrop address list --json`; management commands use the saved address identity.

## Driving it with control-namedrop

Preconditions:

- The debug binary builds and native Herdr is available.
- The helper creates two plain shell panes and an activated disposable workspace.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature names`.
- Naming runs `name @build --type terminal`, then `address list --json`. The saved alias is `build` and the pane type is `terminal`.
- Renaming runs `address rename --input-json` with the returned `id` as `address_id` and `alias` set to `verify-build`. A fresh list must contain the new alias with the same ID and no old alias. Inspect `names.json` and the command transcript.
- Persona editing runs `name @reviewer --type agent --persona 'Reviews Rust changes.'` in the second pane, repeats naming without a persona flag, then lists names. `persona-saved.json` must retain the text. Naming with `--generic` and listing again must produce a null persona in `persona-cleared.json`.
- Finally it asks for a persona on the terminal name. The CLI must refuse with `a named terminal cannot hold a persona`, and the list must still show a null persona.
- For a linked disposable popup instance, invoke `name-agent`, read the returned/created popup pane, enter `@popup-build`, then accept terminal type with Enter. Enter accepts only when the prompt shows a detected `[terminal]` default; otherwise type `terminal` or `t`. List names to prove persistence. Reopen and cancel with Esc, then prove the list is unchanged. The default helper does not drive this entry point: the names recipe never links the candidate as a plugin, so `herdr plugin action invoke name-agent` has nothing to run in its fixture.
- In the manager, use up/down to select, left/right or `<`/`>` to page, Enter to open a name, N to name the invoking pane, R to refresh, T to page the tour while the space has no names, and the displayed menu keys for actions. An agent's menu adds P to edit its persona, and renaming an agent offers the persona editor as an optional second step. Name fields and yes/no confirmations require Enter; Esc returns or closes. Capture each action and confirm it with `address list`. Record this separately from CLI proof.

## Gotchas

- The list's alias omits `@`; the naming response displays it.
- A forced `--type agent` on a shell can save a persona but does not create a registered Codex/Claude session.
- Naming without `--persona` preserves the description. Use `--generic` to clear it.
- Terminal names cannot have personas. The CLI refuses `--persona` on a terminal name, and the naming popup never asks a terminal for one. Popups may offer provider-backed suggestions for real agents; that requires a separate authenticated check.
- The manager removes names rather than panes. Removal can be refused while work is unfinished, and Enter defaults to No on confirmation. To arrange unfinished work, send from a second named pane, because a message addressed only to its author is refused.
