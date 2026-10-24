# Compatibility

## Platforms

| Platform | Status |
|---|---|
| macOS on Apple silicon | Supported. Tested by hand on 2026-09-10. |
| Linux x86_64 (glibc) | Supported. Tested by hand on 2026-09-26. |
| Linux aarch64 (glibc) | Builds and passes CI, but hasn't been tested by hand yet. |

Nobody has tried musl (Alpine), NixOS or other Linux setups.

## Requirements

- Herdr 0.7.5 or newer. Namedrop needs `agent prompt` and `agent send-keys`, which 0.7.4 doesn't have.
- Codex or Claude Code, and Python 3.9 or newer on `PATH`.
- Rust 1.97 or newer to build from source. The prebuilt archive doesn't need Rust.
- On Linux with Codex, agent-status summaries and persona suggestions also need Landlock and the official static Codex binary. `namedrop doctor` tells you what's missing.

## What has been tested

- **macOS, 2026-09-10**, built from source: Herdr 0.8.2, Codex 0.142.2 with gpt-5.5, and Claude Code 2.1.268 with Opus 5. Mentions went both ways between Claude and Codex, and each reply arrived once. Also covered: replacing an agent session, a recipient waiting at an approval prompt, a terminal recipient returning output and an exit code, and installing from GitHub.
- **Personas**, on macOS 26.5 with Herdr 0.9.1, Codex 0.155.1 and Claude Code 2.1.278: both hosts received the persona list in their model requests.
- **Linux x86_64, 2026-09-26**, on the maintainer's laptop.
- **Linux x86_64 (Omarchy), 2026-10-24**, Namedrop 1.2.0 from ericbust/namedrop at main, commit bc70992: Herdr 0.9.1, Codex 0.162.1 with gpt-6.1-sol, and Claude Code 2.1.296 with claude-opus-5-5. Codex's shared app-server warning appeared and cleared after its fix. Mentions went both ways between Claude and Codex, and each reply arrived once. @shell ran a command for Claude.

Herdr 0.7.5 was only checked for loading the plugin and parsing commands. The full runs above used 0.8.2 and 0.9.1.

## Codex 0.162 and later: the shared app-server

Codex runs its sessions through its shared app-server, so Herdr cannot tell which pane each Codex session is in.

Since version 0.162, Codex starts one background app-server for each Codex home (CODEX_HOME) and runs every session's hooks inside it. That server keeps the environment of wherever it was first started, not of the pane you type in. Herdr then files a Codex session under the wrong pane or under none: mentions to that agent stay queued, a second Codex pane's session can be filed under the first, and Namedrop's own Codex hooks go silent. Submitting another prompt or reinstalling the Herdr integration does not help. The server also updates and restarts itself, which can close an open Codex window the next time it saves a setting.

The fix turns the shared app-server off, so each Codex runs inside its own pane, where Herdr can see it. While it is off, the `codex agents` session browser and Codex remote control are unavailable. Namedrop never changes Codex settings: run the steps yourself, or let the agent in the pane run them once you approve.

```sh
codex features disable daemon_auto_start
codex app-server daemon stop
```

Then restart Codex in each of its Herdr panes. The stop is needed because a running server keeps taking new sessions even with the setting off. It disconnects every open Codex window that uses it, including the one whose agent ran these steps for you; relaunch those windows. Run the commands with the same `CODEX_HOME` as the Codex sessions in your panes.

To fix one session only, leave the setting on and start that Codex with `codex --no-daemon`. To undo the fix and turn the shared app-server back on, run `codex features enable daemon_auto_start`.

Namedrop reports this as ND-SESSION-003 in `namedrop doctor`, when you name a Codex pane, during activation, and at the start of an affected Codex session.

## Not tested

- The archives aren't signed or notarized, and nobody has checked how macOS treats a downloaded one.
- Unattended use. Your agent's permission settings can still ask you to approve a command.
- The macOS run above came before guided updates, tracked coordination and personas. Automated tests cover those with stand-in agents, not real ones.
