# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.1.1] - 2026-10-22

### Changed

- Each delivery envelope carries only the rules for the kind of delivery it is: a context request, a result set, a report to the person, a coordination nudge or an ordinary message. The whole rulebook used to ride along on every delivery; the instruction text per envelope is now 57% to 79% shorter. Nothing changes in what an envelope authorizes.

## [1.1.0] - 2026-10-22

### Fixed

- A delivery that Claude Code hands to the hook as marked pasted text, which it does for any prompt over 800 characters, now gets its verification context. Before, a long envelope arrived without it, so the agent could not tell a routed delivery from text the person pasted.

### Changed

- The selection policy arrives once per session through a new SessionStart hook on Claude Code and Codex, and again after a compaction. Each prompt now carries only the roster rows, about 95% less text than before. Restart agent sessions after updating so they load the new hook; a session started before the update keeps working and sees the roster rows without the policy until it restarts. Codex asks once to trust the new hook definition.
- A delivery that needs no visible answer ends with one status line instead of silence, which saves the extra model call Claude Code spends continuing a turn that printed nothing.
- Shorter command guidance in delivery envelopes. Nothing changes in what an envelope authorizes.

## [1.0.2] - 2026-10-12

### Changed

- Updated the bundled dependencies: rusqlite 0.40 (with its bundled SQLite), sha2 0.11, toml_edit 0.25, clap 4.6.7 and libc 0.2.190. Nothing changes in how Namedrop behaves, and no state migration runs.

## [1.0.1] - 2026-10-12

### Fixed

- A message never goes to its own author. Naming yourself among the leading mentions labels the message instead of addressing it, so a worker's tracked reply that starts with its own name now reaches the lead instead of being refused.
- A second Herdr workspace on a checkout that another workspace already has open now attaches to that checkout. Doctor no longer reports ND-INSPECT-001 for it.
- A delivery received by an idle agent settles when its grace period ends, so the messages queued behind it move on without a human prompting the agent. A coordination nudge settles as no reply instead of raising ND-DELIVERY-003.
- An unanswered tracked request raises ND-DELIVERY-003 only when its work has ended without the worker's answer, not while the assignment still awaits that worker.
- A command that names no Herdr workspace no longer reads or writes another workspace's state when two workspaces share a checkout. It takes the workspace from the caller's pane, and it refuses with exit 65 when it can't tell which workspace owns the checkout.
- Doctor reports ND-SETUP-004 when the active release isn't the staged build, such as after a bare `herdr plugin install`, and offers the command that activates it.

### Added

- Versioned releases. Every merge to `main` raises the version, adds a section to this changelog, and publishes a tagged GitHub release with archives for each platform.

## [1.0.0] - 2026-09-24

Initial public release.
