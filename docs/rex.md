# Run Namedrop on Rex

> **Rex support is beta and under active development.** Namedrop 1.12.0 is the first release that includes it. Rex Server is itself a preview, so a new Rex preview can change behavior that Namedrop relies on, and Namedrop's Rex support can change between releases. [Compatibility](compatibility.md#rex-beta) lists the Rex versions that Namedrop accepts and the ones that were tested. [Limits of Rex support](rex-reference.md#limits-of-rex-support) lists what doesn't work on Rex yet.

This guide installs Namedrop in [Rex](https://www.superlogical.com/rex/docs) without Herdr, sets up your agents, and covers the palette, keys, troubleshooting, updates, and removal. It also connects your agents to agents on a second machine. For the palette actions, the files the install writes, Doctor's Rex diagnostics, and the known limits, see [Namedrop on Rex reference](rex-reference.md). The [README](../README.md) describes what Namedrop does once it runs, such as mentions, groups, Agent status, and Channels. If you use Herdr, see [Run Namedrop in Herdr](herdr.md) instead.

On Rex, Namedrop names panes, delivers mentions and replies between agents in one Rex session, and keeps names current through its bridge, a background process that reads Rex's events. Its relay sends a mention to an agent on another machine that runs its own Rex server and its own Namedrop.

## Install Namedrop for Rex

You need:

- macOS on Apple silicon, or Linux on x86_64 or aarch64 with glibc.
- Rex, with its server running. Namedrop needs a Rex server that supports control API revision 12, 13 or 14. `rex version` prints the range that a server supports.
- Python 3.9 or newer. On macOS, `xcode-select --install` installs Python 3 with the Command Line Tools.
- Claude Code, Codex, or both.

Do these steps on each machine that runs agents.

1. Get Namedrop 1.12.0 or later. Download the `.tar.gz` and its `.sha256` file for your platform from the [releases page](https://github.com/ericbust/namedrop/releases), then check and extract it. On Linux, use `sha256sum -c` instead of `shasum -a 256 -c`.

	```sh
	shasum -a 256 -c namedrop-<version>-<target>.tar.gz.sha256
	tar -xzf namedrop-<version>-<target>.tar.gz
	cd namedrop-<version>-<target>
	```

	To build from source instead, you need Git, Rust 1.97 or newer, and a C compiler:

	```sh
	git clone https://github.com/ericbust/namedrop.git
	cd namedrop
	sh scripts/build.sh
	```

2. Open a shell in a Rex pane, and change to the directory from step 1. Outside a Rex pane, put `rex` on your `PATH`, or set `REX_BIN_DIR` to the directory that holds `rex`.
3. Run the installer:

	```sh
	sh scripts/install.sh
	```

	It prints `Namedrop Rex install:` with `new`, `update`, or `adopt`, then the state directory, the launcher, the Rex server, a line that says Rex support is beta, and each file it will install or update. It changes nothing before you answer.

4. Check the `Rex server:` line. It must name the socket of the Rex server you use. Then answer `y` to `Install Namedrop for Rex? [y/N]`. To skip the question, run `sh scripts/install.sh --yes`.
5. Read the report. It lists the module and the `init.lua` line, what it installed, each key and whether it is in effect, the `namedrop` shell command, and the agent steps. It ends with `In the Rex palette, run Activate Namedrop, then Name this pane. Start new agent sessions.`
6. If the report says to put `pcall(require, "namedrop");` on its own line in `init.lua`, do that, then reload Rex's configuration:

	```sh
	rex config reload
	```

	The installer prints the line instead of editing `init.lua` when the file is a symbolic link, when it cannot edit the file, or when you removed the line that it inserted before.

7. Open a new Rex pane, so that `~/.local/bin/namedrop` is on your `PATH`.

Namedrop runs from its own copy of the runtime in the state directory, not from the directory you installed from. [What the install writes](rex-reference.md#what-the-install-writes) lists every file. If the installer stops, its message says why and what to do. [install.sh messages](rex-reference.md#installsh-messages) lists them.

## Activate Namedrop and name your panes

1. In a pane of the Rex session where your agents will run, open the Rex command palette and run **Activate Namedrop**. If an agent is in the Rex session and this machine has not shown the Namedrop tour yet, the tour opens. Otherwise Namedrop Doctor opens and lists anything left to do, such as approving Codex's hooks.
2. In each pane to name, run **Name this pane** from the palette, or press <kbd>Option</kbd>+<kbd>Shift</kbd>+<kbd>Command</kbd>+<kbd>I</kbd> (`alt+shift+cmd+i`). Type the name, then choose the pane type.

If the Rex session is not active yet, **Name this pane** offers to activate it first: `Namedrop is not active in this session. Press Enter to activate it, or Esc to cancel.`

Namedrop's palette actions and keys were tested in the Rex app on Rex Server preview.23 only. See [Limits of Rex support](rex-reference.md#limits-of-rex-support). If the palette does not work for you, use a shell in a Rex pane of the same Rex session:

1. Activate the Rex session:

	```sh
	namedrop activate --input-json '{"confirmed":true}'
	```

2. To name the pane you are in, run `namedrop name @build --type terminal`.
3. To name another pane, such as an agent's, find its ID first. A full ID looks like `block:01a1dc74-dade-78fc-aa4a-fdedd2b0606f`.

	```sh
	rex block ls --short=false
	namedrop name @codex --pane <Codex's block ID>
	```

[Palette actions and keys](rex-reference.md#palette-actions-and-keys) lists every action.

## Start Claude Code and Codex in a Rex pane

1. Start each agent at the shell of its own Rex pane, as the pane's program.
	- Claude Code: start it, or run `/reload-plugins` in a Claude Code that is running.
	- Codex: start it as `codex --no-daemon`, or turn off its shared app-server as [Compatibility](compatibility.md#codex-0162-and-later-the-shared-app-server) describes. Namedrop cannot tell which pane an agent session of the shared app-server runs in, and Doctor reports `ND-SESSION-003` for it. In a new Codex session, approve Namedrop's hooks in `/hooks` if Codex asks.
2. Send each agent one prompt. Its hook then records which pane its agent session runs in. Codex runs its hooks only from its first prompt.
3. Name Claude Code's pane `claude` and Codex's pane `codex`, and keep the pane type `agent`. Use **Name this pane**, or `namedrop name` with `--pane` as in [Activate Namedrop and name your panes](#activate-namedrop-and-name-your-panes).
4. Type a mention at the start of Claude Code's prompt:

	```text
	@codex Reply with exactly the word pong.
	```

	Codex receives the message, and its answer comes back to Claude Code.

If an agent's pane gets no agent session, run **Namedrop Doctor** or `namedrop doctor`. `ND-SESSION-004` says why Namedrop refused to bind the agent session, and `ND-SESSION-003` reports a Codex shared app-server. `ND-SESSION-002` means that Namedrop has no agent session for the agent yet, because its hook has not run or is not installed. Run **Activate Namedrop** again, restart the agent, send it a prompt, and run Doctor again.

## Open Namedrop's screens

Namedrop's screens open in a floating Rex layer over the pane you run them from. Run an action from the `Namedrop` category of the Rex command palette, or press its key:

| Action | Key | Opens |
|---|---|---|
| Name this pane | `alt+shift+cmd+i` | The naming screen for this pane |
| Manage Namedrop names and agents | `alt+shift+cmd+m` | [Manage names](../README.md#manage-names) for this Rex session |
| Agent status | `alt+shift+cmd+s` | [Agent status](../README.md#agent-status) for this pane |
| Namedrop Doctor | `alt+shift+cmd+h` | Doctor's report for this Rex session. See [Troubleshooting](#troubleshooting) |
| Namedrop tour | `alt+shift+cmd+o` | The [tour](../README.md#tour) |
| Open Namedrop Channels | None | [Channels](../README.md#channels-beta) in your browser. See [Channels on Rex](../README.md#channels-on-rex) for what differs |

On a Mac, `alt+shift+cmd+i` is Option-Shift-Command-I. The module binds a key only when no other binding holds it when Rex loads the module, and a key that `init.lua` binds after the Namedrop line wins over Namedrop's. The install report and Doctor's `ND-REX-001` say whether each key is in effect.

To run an action without the palette or a key, run `rex do namedrop_<name>` in a Rex pane, such as `rex do namedrop_doctor`. [Palette actions and keys](rex-reference.md#palette-actions-and-keys) lists every action and its name.

## Pause, resume, or deactivate Namedrop

Each of these acts on the current Rex session and keeps its names and messages:

- **Pause Namedrop** stops all collaboration in the Rex session. Messages wait until you resume, and Doctor shows `ND-SPACE-001` while the session is paused.
- **Resume Namedrop** starts it again.
- **Deactivate Namedrop** turns Namedrop off in the Rex session. Run **Activate Namedrop** to turn it back on.

Run them from the palette, or from a shell in a pane of the Rex session:

```sh
namedrop space pause
namedrop space resume
namedrop deactivate
```

To remove Namedrop from the machine, see [Remove Namedrop from Rex](#remove-namedrop-from-rex).

## Troubleshooting

Run **Namedrop Doctor** from the palette, press `alt+shift+cmd+h`, or run `namedrop doctor` in a shell in a pane of the Rex session. Doctor's exit codes and screen work as [Troubleshooting](../README.md#troubleshooting) in the README describes. These are the Rex problems you are most likely to see. [Doctor on Rex](rex-reference.md#doctor-on-rex) gives the full text of each code.

| Doctor reports | What to do |
|---|---|
| `ND-SETUP-001`, Namedrop is not active in this Rex session | Run **Activate Namedrop** |
| `ND-REX-001`, the Rex module is not confirmed | If an older module is still loaded, run `rex config reload`. Otherwise run `sh scripts/install.sh` again. Then run Doctor again |
| `ND-SESSION-002`, no session identity for a pane | Send the agent a prompt. If it still has none after a turn, run **Activate Namedrop** again and restart the agent |
| `ND-SESSION-003`, a Codex session runs through Codex's shared app-server | Start Codex as `codex --no-daemon`, or follow [the shared app-server fix](compatibility.md#codex-0162-and-later-the-shared-app-server) |
| `ND-SESSION-004`, Namedrop refused to bind the agent session | Start the agent as the pane's own program, at its shell, then send it one prompt |
| `ND-INTEGRATION-003`, a hook wrapper does not name its agent | Run **Activate Namedrop** again, then restart the agent |
| `ND-SPACE-001`, Namedrop is paused in this Rex session | Run **Resume Namedrop** |
| `ND-UPDATE-002`, update checks are disabled by invalid settings | Set `check_for_updates` to `true` or `false` in `~/.config/namedrop/settings.json` |

Other problems:

- **The palette lists no Namedrop actions.** Rex has not loaded the module. Check that `init.lua` has `pcall(require, "namedrop");` on its own line, run `rex config reload`, then run `namedrop doctor` and read `ND-REX-001`.
- **A key does nothing.** Another binding holds it. Run the action from the palette or with `rex do`, and see [Open Namedrop's screens](#open-namedrops-screens).
- **An action fails.** `Namedrop's launcher <path> is missing. Run sh scripts/install.sh again.` means what it says. `<title> did not finish within 25 s, so Rex stopped it.` usually means an update is running, so try again when it is done. [Palette actions and keys](rex-reference.md#palette-actions-and-keys) lists every action error.
- **The `namedrop` command is not found.** Open a new Rex pane. The installer adds `~/.local/bin` to `PATH` in your zsh or bash startup files only. For another shell, add `~/.local/bin` to `PATH` yourself.
- **The installer stops.** Its message says why and what to do. [install.sh messages](rex-reference.md#installsh-messages) lists them.
- **The palette does not work at all.** Use the shell commands in [Activate Namedrop and name your panes](#activate-namedrop-and-name-your-panes).

When you [report a problem](https://github.com/ericbust/namedrop/issues), include the output of `rex version` and `namedrop --version`, your platform and agent versions, the steps to reproduce, and Doctor's report. Remove credentials and private conversation text first.

## Update Namedrop

An install that `install.sh` made checks for a new release once a day. When one is available, your agents show this at the start of an agent session, and Doctor shows it as `ND-UPDATE-001`:

```text
Namedrop <version> is available (you have <installed>). Update: download Namedrop <version> and run sh scripts/install.sh
```

The check offers only releases that include Rex support, which are 1.12.0 and later.

To update:

1. Close any Namedrop screen that is open in Rex, such as the tour or Doctor.
2. Get the new version. In a checkout, run `git pull`, then `sh scripts/build.sh`. For a release archive, download and extract the new release, as in [Install Namedrop for Rex](#install-namedrop-for-rex).
3. Run `sh scripts/install.sh` from the checkout or the new directory. It prints `Namedrop Rex install: update`, and keeps your names and messages.
4. Start new agent sessions.

To turn off the check, create `~/.config/namedrop/settings.json` with this content:

```json
{"check_for_updates": false}
```

On a Mac without the developer tools, the check does not run, because Apple's `/usr/bin/python3` and `/usr/bin/git` only offer to install them. Run `xcode-select --install` to turn the check on. [Updates](rex-reference.md#updates) gives the full rules.

If Herdr owns your Namedrop install, as it does after [Herdr first, then Rex](#herdr-first-then-rex), update it with **Update Namedrop** in Herdr. That updates Namedrop for both Herdr and Rex.

## Remove Namedrop from Rex

1. Close every Namedrop screen that is open in Rex. Removal waits 60 seconds for an open screen, then fails with `Namedrop on Rex is busy`.
2. In a Rex pane, run:

	```sh
	namedrop install --remove --input-json '{"confirmed":true}'
	```

For an install that `install.sh` made, removal deletes the Rex configuration, the agent packages, and the `namedrop` shell command, and prints `Namedrop was removed from Rex. Its state directory was preserved.` It keeps the state directory with your names and messages, the newest `init.lua` backup, and the `PATH` lines in your shell startup files. [namedrop install](rex-reference.md#namedrop-install) lists what removal deletes. To install again, run `sh scripts/install.sh`. An install into the same state directory brings back your names.

If Herdr owns the install, removal deletes only the Rex module and the `init.lua` line, and prints `Removed only the Rex configuration. The shared packages and install record belong to another owner.` To remove Namedrop from Herdr too, follow [Uninstall from Herdr](herdr.md#uninstall).

## Use Herdr and Rex on one machine

A machine has one Namedrop: one state directory, one runtime, and one set of agent packages, which Herdr and Rex share. Herdr's install owns them once Herdr has Namedrop. Until then, the install that `install.sh` made owns them.

### Run Herdr inside Rex

You can run Herdr as the program of a Rex pane and use Namedrop in Herdr's panes as usual. For a Herdr pane, Namedrop uses Herdr. See [Herdr inside Rex](compatibility.md#herdr-inside-rex).

### Herdr first, then Rex

Run `sh scripts/install.sh` as in [Install Namedrop for Rex](#install-namedrop-for-rex). It finds Herdr's install and prints `Namedrop Rex install: adopt` with Herdr's state directory. It installs the Rex module with Herdr's runtime, and publishes no runtime of its own. Herdr's updates then update Namedrop for both Herdr and Rex.

The installer adopts Herdr's install only while Herdr lists Namedrop as an installed plugin. If the plugin list it reads does not name Namedrop, the installer stops with this message:

```text
Namedrop's install record names Herdr's Namedrop state (<dir>), but Herdr's plugin list in <file> does not name Namedrop. If Herdr uses another config directory, run sh scripts/install.sh again with Herdr's XDG_CONFIG_HOME. Otherwise install Namedrop in Herdr again, or move that state directory away, then run sh scripts/install.sh again.
```

If you run a debug Herdr, or Herdr has a different `XDG_CONFIG_HOME` from your shell, run the installer with Herdr's `XDG_CONFIG_HOME` before you move anything. If you removed Namedrop from Herdr, move that state directory away, and the installer makes a new install of its own.

The installer also refuses, and changes nothing, in these cases:

- Herdr's install has not written its install record yet.
- Herdr runs a Namedrop release without Rex support.
- A move from Rex to Herdr stopped part way.
- The state directory that the installer would use belongs to Herdr.

Each message says what to do in Herdr. [install.sh messages](rex-reference.md#installsh-messages) gives the texts.

### Rex first, then Herdr

When you activate Namedrop in Herdr, Herdr's install takes over the Rex install. It stops the Rex install's background processes, such as its bridge, takes over the agent packages and the Rex module, and keeps the old state directory. Herdr's setup prints a notice:

```text
The names made on Rex stay in <old state> and were not carried over. In each Rex session, run Activate Namedrop again and name the panes again.
```

After the move:

1. In each Rex session, run **Activate Namedrop** again, and name the panes again.
2. Restart an agent that shows Hook failed or does not answer a mention. An agent turn that was running during the move can show Hook failed. Other agents run Herdr's hooks at their next turn.

Until each agent from before the move takes a turn or ends, Herdr's Doctor reports `ND-INSTALL-001`, "Agents on Rex have not run the new hooks yet." It lists each agent's pane. The old install's commands refuse with the short form of `ND-INSTALL-001`, which names Herdr's state directory. [ND-INSTALL-001](rex-reference.md#nd-install-001) gives both texts.

If you add a Herdr release without Rex support after a Rex install, Doctor on Rex reports `ND-INSTALL-003`. Update Namedrop in Herdr, then run **Activate Namedrop** in Herdr again.

## Mention an agent on another machine

In the rest of this guide, A is your machine and B is the machine whose agent you mention. Set up B first, as [Set up the machine that receives](#set-up-the-machine-that-receives) describes.

1. Add B to A's Rex hosts. Pick a label of 1 to 64 ASCII letters, digits, `-`, and `_`. The label is the part of the address after the second `@`.

	```sh
	rex hosts add devbox ssh://<user>@<host>
	```

	Namedrop has been tested only with an `ssh://` endpoint. Rex takes other forms too, such as `https://<host>.<tailnet>.ts.net` for a server on Tailscale. If you set up B with `rex server install ssh://`, B is already in your hosts.

2. Check that A can reach B:

	```sh
	rex hosts check devbox
	```

3. Make sure that `rex` is on the `PATH` of Namedrop's commands on A. The relay runs `rex` to reach B. A Rex pane has it. A shell outside Rex may not.
4. The relay works only while A's Rex server runs on Rex's default socket. The relay reads A's hosts with `rex hosts list`, which ignores `-S` and `REX_SERVER`. It reads your Rex home and the Rex server on the default socket, and your home is that server unless you chose another with `rex home set` or `REX_HOME`. With A's server on another socket, each remote send fails with `could not read your Rex hosts: <error>`.
5. Ask a named agent on A to send the message. In Claude Code's prompt, type:

	```text
	Run this command: namedrop send --json --input-json '{"author_kind":"agent_session","request_id":"pong-1","source":"@codex@devbox Reply with exactly the word pong."}'
	```

	The command prints JSON. Its `data.relayed` list has one entry for each remote recipient, and the entry's `statement` says `@codex@devbox: queued for the Namedrop on devbox, which delivers it there.`

6. Wait for the reply. A collects B's replies every 30 seconds while a thread with B is open. A thread stays open for 24 hours after the last message that B accepted or the last reply that A collected. The reply arrives in Claude Code from `@codex@devbox`, in the thread of your message.

To see the reply, send from an agent, not from a terminal. Namedrop saves a reply to a terminal in its thread and does not print it in the terminal. A terminal sends the same message without `"author_kind":"agent_session"`. From a terminal, a send with that field fails with `an agent-authored message requires a current Agent Session`.

A remote address works only at the start of the `source` of `namedrop send`. Typed at the start of an agent's prompt, `@codex@devbox` is not a mention, and Namedrop sends nothing. The label must match a label that `rex hosts list` prints, letter for letter. Otherwise the send fails at once with `devbx is not in your Rex hosts (rex hosts list): devbox`, and nothing is queued.

When A's agent answers a reply from `@codex@devbox`, its answer goes back to `@codex` on B in the same thread. A message to a remote agent carries text only. It cannot attach context or tracked work.

## Set up the machine that receives

Do these steps on B, the machine whose agent A mentions. B runs Linux or macOS. Windows is out of scope.

1. If B is a Linux machine that you reach with ssh and that has no Rex yet, set up its Rex server from A:

	```sh
	rex server install ssh://<user>@<host> --label devbox
	```

	The command installs Rex on B, sets up B's server to start on its own, and adds B to A's hosts as `devbox`. It supports only a Linux B with systemd. Run `rex server install --help` for its options. Namedrop has not been tested on a server that this command set up.

2. On B, install Namedrop as [Install Namedrop for Rex](#install-namedrop-for-rex) describes, with B's Rex server running. Run the installer on B itself, because it edits and reloads the configuration of the local Rex server only. In an ssh shell on B:
	- `rex` may not be on your `PATH`. Set `REX_BIN_DIR` to its directory, such as `~/.local/bin`.
	- On Linux, the installer looks for B's Rex server at `$XDG_RUNTIME_DIR/rex/server.sock`. If your shell has no `XDG_RUNTIME_DIR`, set `REX_SERVER` to the socket of B's server. Without `XDG_RUNTIME_DIR`, Rex puts it at `~/.local/state/rex/run/server.sock`.
3. Open a pane on B. From A, run `rex -S devbox new agents`, which creates a Rex session named `agents` on B with one shell pane. The `rex` command does not show a pane in your terminal. `rex -S devbox send` and `rex -S devbox send-key` type into the pane, and `rex -S devbox capture` prints its screen. For another pane, run `rex -S devbox split`, which adds a pane and moves the focus to it. `rex -S devbox block ls --short=false` lists the panes, and `-b <block ID>` selects one for `send`, `send-key` and `capture`. A Rex app connected to B can also show B's panes. Namedrop has not tested that.
4. Start the agents in panes of B's Rex server, as in [Start Claude Code and Codex in a Rex pane](#start-claude-code-and-codex-in-a-rex-pane). Activate the Rex session and name the agents with `namedrop` in a shell pane on B, as in [Activate Namedrop and name your panes](#activate-namedrop-and-name-your-panes).
5. Name the agent in only one of B's Rex sessions. If the name is in two of them, B answers that the mention is ambiguous and names both Rex sessions.
6. Keep B's bridge running. Rex starts it when B's server loads the Namedrop module, and any Namedrop command or hook in one of B's panes starts it too. While the bridge is stopped, A keeps the message and tries again. After 24 hours, A gives up and tells the sender `devbox was unreachable for 24 hours`.
7. Over `ssh://`, log in to B as the user who runs B's Namedrop. B takes a message only from a caller that Rex identifies as a Tailscale principal, or as a local peer with B's own user ID. Over `ssh://`, Rex identifies A as a local peer with the user ID that A logged in as.
8. If B's agents must start new conversations with agents on A, add A to B's Rex hosts. B does not need A in its hosts to answer, because A collects B's replies.

B's Namedrop delivers a relayed mention as it delivers a local one. It types the message only when the agent's turn has ended, so a mention never interrupts B's agent mid-turn.

Unless Rex confirms which machine sent a message, the sender's host is the name that A gave itself. Then, before the first message of each thread, B's agent gets this notice: `[Namedrop: laptop is the name the sending machine gave itself, which Rex did not confirm.]` A Tailscale caller is named by its device instead, with no notice.

If B's agent answers the notice before B has tried to deliver the message to it, that answer stays on B and is not relayed to A. An answer to the notice after that point reaches A as the agent's reply to the message.

## Let mentions from other machines reach a terminal

By default, B relays a mention only to an agent. A mention from A to a terminal on B fails, and the sender gets a notice in its thread: `Namedrop could not relay this message to @bterm@devbox: @bterm is a terminal on <B>, and <B> relays mentions only to agents.` Here `<B>` is B's host name up to its first dot, not A's label for B.

A terminal runs each new message as a shell command. So a relayed mention to a terminal on B runs a command on B, under your account, that someone wrote on A. Turn this on only if you want the people and agents on A to run commands on B.

To let relayed mentions reach B's terminals, create the file `relay/allow-terminals` in B's state directory. The installer printed the directory on its `State directory:` line on B:

```sh
state='<the directory on the State directory line>'
mkdir -p "$state/relay"
touch "$state/relay/allow-terminals"
```

If you did not note the line, [Files in the state directory](rex-reference.md#files-in-the-state-directory) says how Namedrop picks the directory. B reads the file for each new message, so you do not need to restart anything. A message that B refused before you created the file stays refused. Send it again. The terminal's output and exit code come back to the sender as a reply.

To turn it off again, delete `relay/allow-terminals`.

## Resume a thread that the Automatic Reply Limit paused

The Automatic Reply Limit counts replies from another machine as it counts local ones. After 10 agent replies with no message from a person, A pauses the thread. A then holds the next replies from B instead of delivering them, and tells the sender once in its thread:

```text
The Automatic Reply Limit paused this thread, so Namedrop is holding the replies from @codex@devbox. A person's message in this thread, or Continue, resumes it, and the held replies follow.
```

To resume the thread, Continue it:

1. On A, run `namedrop doctor` in a pane of the Rex session.
2. Find `ND-THREAD-001`, "A collaboration thread is waiting for approval." Its repair is a `thread continue` command with the thread's ID.
3. Run that command. It has this form:

	```sh
	namedrop thread continue --input-json '{"thread_id":"<thread ID>"}'
	```

Your own message in the thread resumes it too. The held replies then arrive in the order that B sent them. Each Continue allows 10 more replies, so a long run of held replies can pause the thread again. In that case, continue it again.

A holds at most 20 replies for one thread, and at most 100 for one host label on one local Rex server. B keeps the rest until A has room. Doctor's `ND-RELAY-003` tells you when a hold is full. A held reply that waits 7 days is let go, and the sender is told how many.

B's Namedrop applies the same limit to B's agents in B's thread. When B's limit pauses the thread, A keeps the message and tells the sender that a person on B must resume the thread there with a message or Continue. If nobody resumes it within 24 hours after A queued the message, the message fails with `the Automatic Reply Limit kept its thread on devbox paused for 24 hours`.

## Check the relay

Run `namedrop doctor` on each machine. On A, `ND-RELAY-002` counts messages that did not reach another machine, and `ND-RELAY-003` reports replies that need a look. On B, `ND-RELAY-001` counts replies that expired before A acknowledged them. [Doctor on Rex](rex-reference.md#doctor-on-rex) gives the full texts.
