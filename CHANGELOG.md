# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.1.0] - 2026-10-20

### Added

- Mention agents in other Herdr workspaces. Put a workspace label before the mentions, as in `#exp-dashboard @lead Status?`, name several labels, or use `#all @lead` to reach that name in every workspace. Labels ignore case, a space matches a hyphen, and every workspace also answers to its Herdr id, such as `#w12`.
- A mention without a label stays in the current workspace when the name exists there. If only one other workspace has the name, the prompt goes there and Herdr shows where it went. If several do, nothing is sent and your agent asks which one you meant.
- Replies come back to the workspace that sent the message, and the recipient sees which workspace it came from. Stopping or pausing the conversation on either side applies to both.
- The prompt roster lists the named agents in your other workspaces, and `address list` with `{"workspaces":"all"}` shows every workspace's names and groups.

### Changed

- Namedrop's state moves to schema 6 the first time the new release opens it. Context handoffs and tracked coordination still stay inside one workspace.

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
