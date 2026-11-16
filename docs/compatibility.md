# Compatibility

## Platforms

| Platform | Status |
|---|---|
| macOS on Apple silicon | Supported. Tested by hand on 2026-09-10. |
| Linux x86_64 (glibc) | Supported. Tested with real agents on 2026-11-08. |
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
- **Linux x86_64 (Omarchy), 2026-10-26**, Namedrop 1.4.0 from the release branch: Herdr 0.9.1, Codex 0.164.0 with gpt-6-sol, and Claude Code 2.1.310 with claude-opus-5-5. Codex's shared app-server warning appeared and cleared after its fix, and a second run used `--no-daemon`. Mentions went both ways between Claude and Codex, and each reply arrived once. @shell ran a command for Claude. Mentions to a second workspace, including `#all`, arrived there and their replies came back. A 1.3.3 installation with saved messages upgraded in place: its state moved from schema 5 to 6 and kept every message, and 1.3.3 then refused the migrated state.
- **Linux x86_64 (Omarchy), 2026-10-28**, Namedrop 1.4.2 from the `chore/remove-dashboard` branch, tested at commit c07f928 before the version bump: Herdr 0.9.1, Codex 0.164.0 with gpt-6-sol, and Claude Code 2.1.310 with claude-opus-5-5. Codex's shared app-server warning appeared and cleared after its fix. Mentions went both ways between Claude and Codex, and each reply arrived once. @shell ran a command for Claude.
- **Linux x86_64 (Omarchy), 2026-11-07**, Namedrop 1.6.0 from the `feat/message-efficiency` branch at commit 7d0aa09: Herdr 0.9.1, Codex 0.167.0 with gpt-6-sol, and Claude Code 2.1.319 with claude-opus-5-5. Codex's shared app-server warning appeared and cleared after its fix. Mentions went both ways between Claude and Codex, and each reply arrived once. @shell ran a command for Claude.
- **Linux x86_64 (Omarchy), 2026-11-09**, Namedrop 1.7.0 from the `feat/pointer-prompts` branch on top of 1.6.5, at commit 13d91ae: Herdr 0.9.1, Codex 0.167.0 with gpt-6-sol, and Claude Code 2.1.319 with claude-opus-5-5. Codex's shared app-server warning appeared and cleared after its fix. Mentions went both ways between Claude and Codex as pointer prompts, each settled by the agent's final message with no settle command, and each reply arrived once. @shell ran a command for Claude. Channels posts, a thread reply, direct messages and posts naming another agent reached both agents, the named agent's answer arriving inside the pointer, and a Codex session with no prompt hook read its details through `delivery inbox`. The daily-use lane at commit 217c60a passed 13 of 14 checks; the held-turn check was inconclusive because the Claude helper answered before Codex's turn ended. A 1.6.5 profile, at commit 1bd27f9, upgraded live with both agents left running; Codex sessions started before the update lost their hooks, as with any plugin change.

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

## Rex (experimental)

Rex support is experimental. On Rex, Namedrop does not need Herdr: `sh scripts/install.sh` installs it. See [Run Namedrop on Rex](rex.md).

Namedrop speaks Rex's control protocol, `rex.control.v1`, at API revisions 12, 13 and 14. It offers revisions 12 to 14, and the server answers with the highest revision that both support. Namedrop refuses any other answer with `the server's welcome is not one for rex.control.v1 revisions 12 to 14`. `rex version` prints a server's revision and the range it supports.

| Rex | API revision | Status |
|---|---|---|
| `0.1.0-preview.14`, commit `7e460d0` | 12, supports 0 to 12 | Supported, at revision 12. The spike and the adapter's design probes ran on it |
| `0.1.0-preview.18`, commit `baf79e0` | 13, supports 0 to 13 | Supported, at revision 13. The installer, the palette actions, both install orders, and agents were tested on it, and the relay to a Linux host in the remote-host lab |
| `0.1.0-preview.19`, commit `ad5a836` | 14, supports 0 to 14 | Supported, at revision 14. Tested as preview.18 was, and with Herdr inside Rex |

Rex has been tested with real agents only on macOS on Apple silicon. A Linux Rex server was tested only as the receiving machine in the remote-host lab, in a Docker container, with no real agent. The testbed's Rex lanes use the `rex` that the testbed finds, and their logs do not record its version, except where an entry below names it.

### What has been tested on Rex

- **Rex preview.14, 2026-11-11 to 2026-11-13**, in scratch Rex servers: Claude Code 2.1.322 and 2.1.323 on Haiku, and Codex 0.163.0 with and without `--no-daemon`. A prompt pasted into a block arrived as one draft and submitted in both agents, and both agents reported their status to Rex with no integration installed.
- **The adapter and the bridge**, on the testbed's Rex lane. Claude Code on Opus 5.5 and Codex `--no-daemon` were named through the CLI in one session. A mention typed in Claude Code reached Codex, and Codex's answer came back, with no manual command. A closed named block lost its name within a second. A mention queued to a working Codex was delivered a second after Codex reported `idle`. After a server restart, the mail in flight to both replaced agents was held, and a new Claude Code in the same block bound and was named again.
- **The relay**, with two scratch Rex servers on one Mac standing in for two machines. Claude Code on Haiku 5.5 on A mentioned Codex `--no-daemon` on B, and a four-message conversation went back and forth. A retried send was accepted once. With B's bridge stopped, A kept the message until the bridge started again. A refused to type into B's blocks through a forwarded socket. Later checks without a model, up to commit `7c89d9a`, covered `relay/allow-terminals`, the Automatic Reply Limit pausing a relayed thread and holding its replies until `thread continue`, and B and A on different relay builds.
- **Rex preview.14 and preview.18, 2026-11-14**, Namedrop at commit `329c278b`, which offers only revision 12, in scratch Rex servers, without agents. Both servers answered with revision 12. On both, Doctor reported `ND-SETUP-001`, `activate` succeeded, and a terminal block was named.
- **Rex preview.14 and preview.18, 2026-11-14**, Namedrop at commit `a08c40b6`, which offers revisions 12 to 13, in scratch Rex servers, without agents. Rex preview.14 answered with revision 12, and preview.18 answered with revision 13. On both, the four live tests that need no fixture passed, Doctor reported only `ND-SETUP-001`, `activate` succeeded, and a terminal block was named.
- **Rex preview.14, preview.18 and preview.19, 2026-11-14**, Namedrop at commit `02fa5127`, which offers revisions 12 to 14, in scratch Rex servers. The servers answered with revisions 12, 13 and 14. On each, the four live tests passed, Doctor reported only `ND-SETUP-001`, `activate` succeeded, a terminal block was named, and a root status set and cleared after attachment reached the bridge. The testbed's Herdr-inside-Rex lane passed with preview.19. On every build, Rex sometimes sends no startup events for a new session's first block; activation reads the block itself, so nothing is lost. See [Control API revisions](../experimental/rex/ADAPTER.md#control-api-revisions).
- **The installer and the palette actions, 2026-11-15**, Namedrop 1.7.9 at commit `a117053e`, on Rex preview.18 and preview.19. The testbed's Rex lane installed the build's release archive with `sh scripts/install.sh --yes`, with no `herdr` on its `PATH`. The server listed the eleven Namedrop actions. `rex do --origin palette namedrop_activate` opened the tour in one floating layer, and `namedrop_name` named Claude Code and Codex from the names typed into each layer. A mention each way between them got its reply back. All 17 checks passed on both builds.
- **Install orders and removal**, on preview.18 at commit `164d7daf` and on preview.19 at commit `35a7abaf`, with real Herdr 0.9.5, Codex 0.163.0 on gpt-6-sol, and Claude Code. With Herdr first, `install.sh` adopted Herdr's install. Beside Namedrop 1.11.1, it refused and changed no file, and adopted after Herdr updated. With Rex first, Herdr's activation took over the Rex install, the old runtime refused, and Doctor's `ND-INSTALL-001` counted the agents from before the move. `install --remove` after each order restored `init.lua` byte for byte. A Codex named from the palette on a second Rex server answered a mention relayed through `rex hosts`. All 92 checks of the five scenarios passed.
- **The remote-host lab, 2026-11-15**, at commit `49c29139`: a scratch macOS Rex server as A, on preview.18, and a Debian trixie container with Superlogical's apt Rex package as B, reached over `ssh://`. B ran preview.18, and in an earlier run preview.19. No model ran. Terminals on both sides exchanged a relayed command and its reply, A kept a message while B was away in four ways, and a retried call was accepted once. [What has not been tested](rex-reference.md#what-has-not-been-tested) lists what the lab proved and what it did not.

Not recorded on Rex: the relay with real agents on preview.18 and preview.19. The testbed's Rex lanes do not log their Rex version, so their runs do not show which preview they used. Not tested on Rex: TLS to a remote Rex server, such as through Tailscale, a real Linux host, a host that `rex server install ssh://` set up, Mac to Mac across a network, real agents on a Linux host, the Rex app's own command palette and keys, a restart of the Rex app as opposed to the server, and Windows, which is out of scope.

### Herdr inside Rex

You can run Herdr as the program of a Rex block and use Namedrop in Herdr's panes as usual. Herdr's panes inherit the block's `REX_*` variables, so Namedrop chooses between the two. It uses the terminal host whose server is the nearest ancestor of the process. For a Herdr pane, that is Herdr. When Namedrop is in doubt about Herdr, it uses Herdr.

Tested on macOS on 2026-11-14 at commit `3c18cb9`: Herdr 0.9.5 as the program of a block of a scratch Rex server, Codex 0.163.0 with gpt-6-sol, and Claude Code 2.1.325 with claude-sonnet-5-5. The testbed checklist passed 10 of 10, and Namedrop never connected to the Rex server as a client.

## Not tested

- The archives aren't signed or notarized, and nobody has checked how macOS treats a downloaded one.
- Unattended use. Your agent's permission settings can still ask you to approve a command.
- The macOS run above came before guided updates, tracked coordination and personas. Automated tests cover those with stand-in agents, not real ones.
