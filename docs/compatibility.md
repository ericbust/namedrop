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

Herdr 0.7.5 was only checked for loading the plugin and parsing commands. The full runs above used 0.8.2 and 0.9.1.

## Not tested

- The archives aren't signed or notarized, and nobody has checked how macOS treats a downloaded one.
- Unattended use. Your agent's permission settings can still ask you to approve a command.
- The macOS run above came before guided updates, tracked coordination and personas. Automated tests cover those with stand-in agents, not real ones.
