# Contributing to Namedrop

Namedrop does one thing: you name agents, and other agents mention them in a prompt. Changes should make that work better, not add a second way to do it.

## Setup

Install Rust 1.97 or newer, clone the repository, and run:

```sh
cargo test --locked --all-targets
```

Namedrop runs on macOS on Apple silicon and Linux x86_64 (glibc), inside Herdr 0.7.5 or newer, with current Herdr native integrations and agent sessions started inside Herdr. Linux aarch64 builds pass CI but haven't been tested by hand. See [compatibility](docs/compatibility.md) for tested versions.

## Before opening a pull request

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --test codex_install codex_policy_checker_accepts_only_installed_collaboration_commands -- --ignored --exact
CARGO_TARGET_DIR=target/crate-checks cargo package --locked --allow-dirty
sh scripts/package-release.sh
```

CI runs all of these on `macos-15`, `ubuntu-24.04` and `ubuntu-24.04-arm`, except the `codex_install` test and `--allow-dirty`. That test is skipped by default and needs a real `codex` on `PATH`. If you don't have codex-cli, skip it and say so in your pull request.

On Linux you also need zsh, because `tests/shell_command.rs` runs `/bin/zsh`, and glibc's static `libc.a`, because the Landlock test in `tests/linux_sandbox.rs` links with `cc -static`.

The native Herdr tests, such as `tests/seamless_native.py`, `tests/coordination_native.py`, `tests/personas_native.py`, and `tests/checkout_context_native.py`, don't run in CI. They use the shell named in `NAMEDROP_TEST_SHELL` (`bash` or `zsh`, zsh by default when it's installed), so run them with each shell your change affects.

`Cargo.toml`, `Cargo.lock` and `herdr-plugin.toml` must have the same version, and each pull request raises it and adds a section to `CHANGELOG.md`. Run `sh scripts/release.sh check origin/main` to check all of that before you push; the Release check workflow runs the same command. `scripts/package-release.sh` also exits with 65 if the binary's `--version` doesn't match.

## Guidelines

- Keep changes small, and add a regression test for any change in behavior.
- Don't commit `target/` or local Namedrop state.
- Tests use temporary homes and state and fake Herdr executors. Never install a test build into your real profile.
- Keep `cargo package` checks in their own `CARGO_TARGET_DIR`. If a test binary looks stale, check which source path it was built from.
- When you report test results, say which commit and binary you tested.
- For handoff or group changes, start with `cargo test --locked --test handoffs`. Keep [operations](docs/operations.md#handoffs-and-groups), the schemas and the installed skill in step with each other.
- For tour changes, edit `tours/<id>.md` and run `cargo test --locked --test tour`, which fails if a page won't fit the popup. [Tour presentations](docs/operations.md#tour-presentations) describes the format and the once-only marker.
- Releases follow [the release checklist](docs/releasing.md), which includes a test with real agents that CI can't run.

## Behavior to keep

These are easy to break by accident:

- A mention at the very start of a prompt sends the prompt unchanged.
- A mention later in a prompt stays with the current agent, which delegates while keeping your goal.
- A mention that doesn't match a name stays in the pane, and nothing retries it.
- Deliveries go out in order, and each one ends exactly once. If Namedrop can't tell whether a delivery started, it holds it instead of retrying.
- Text written by another agent never grants permissions.
- Waking an agent for new mail never treats an attempt that's still running as crashed.
- Reading or previewing a handoff never sends anything, finishes work, grants context or captures Git state.
- Reading partial results doesn't use up the final combined answer, and a request's content stays with the session that sent it.
- A group resolves to its agents all at once. Editing, renaming or deleting it never changes who an existing request goes to.
- Freshness compares against the pinned evidence and says `unknown` when it can't tell.
- Context rules cover resolved file paths, directory and historical Git reads, exact human reveal, and byte and line limits.
- File and Git context comes from the checkout a snapshot was published with: the workspace's own worktree by default, or a registered worktree of the same repository that the sender named and Namedrop validated. It never comes from the directory a command runs in, and a later read never swaps in another worktree. Text-only and pane-only snapshots with Git capture off don't need a worktree.
- Schema migrations only add. Existing messages, deliveries, configuration and request IDs survive them.
- Installing, loading into an agent, and a working round trip are three separate results. Don't report one as another.
