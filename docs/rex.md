# Run Namedrop on Rex

Rex support is experimental. This guide installs Namedrop in [Rex](https://www.superlogical.com/rex/docs) without Herdr, sets up your agents, and connects them to agents on a second machine. For the palette actions, the files the install writes, Doctor's Rex diagnostics, and the known limits, see [Namedrop on Rex reference](rex-reference.md). For the Rex versions that Namedrop accepts, see [Compatibility](compatibility.md#rex-experimental).

On Rex, Namedrop names panes, delivers mentions and replies between agents in one Rex session, and keeps names current through its bridge, a background process that reads Rex's events. Its relay sends a mention to an agent on another machine that runs its own Rex server and its own Namedrop.

## Install Namedrop for Rex

You need:

- macOS on Apple silicon, or Linux on x86_64 or aarch64 with glibc.
- Rex, with its server running.
- Python 3.9 or newer. On macOS, `xcode-select --install` installs Python 3 with the Command Line Tools.
- Claude Code, Codex, or both.

Do these steps on each machine that runs agents.

1. Get Namedrop. Use one of these:
	- A release archive. Download the `.tar.gz` and its `.sha256` file for your platform from the [releases page](https://github.com/ericbust/namedrop/releases), then check and extract it. On Linux, use `sha256sum -c` instead of `shasum -a 256 -c`.

		```sh
		shasum -a 256 -c namedrop-<version>-<target>.tar.gz.sha256
		tar -xzf namedrop-<version>-<target>.tar.gz
		cd namedrop-<version>-<target>
		```

	- A checkout. Building needs Git, Rust 1.97 or newer, and a C compiler. Until a release includes Rex support, build the `experimental` branch.

		```sh
		git clone https://github.com/ericbust/namedrop.git
		cd namedrop
		git switch experimental
		sh scripts/build.sh
		```

2. Open a shell in a Rex pane, and change to the directory from step 1. Outside a Rex pane, put `rex` on your `PATH`, or set `REX_BIN_DIR` to the directory that holds `rex`.
3. Run the installer:

	```sh
	sh scripts/install.sh
	```

	It prints `Namedrop Rex install:` with `new`, `update`, or `adopt`, then the state directory, the launcher, the Rex server, and each file it will install or update. It changes nothing before you answer.

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

1. In a pane of the Rex session where your agents will run, open the Rex command palette and run **Activate Namedrop**. If an agent is in the session and this machine has not shown the Namedrop tour yet, the tour opens. Otherwise Namedrop Doctor opens and lists anything left to do, such as approving Codex's hooks.
2. In each pane to name, run **Name this pane** from the palette, or press <kbd>Option</kbd>+<kbd>Shift</kbd>+<kbd>Command</kbd>+<kbd>I</kbd> (`alt+shift+cmd+i`). Type the name, then choose the pane type.

If the session is not active yet, **Name this pane** offers to activate it first: `Namedrop is not active in this session. Press Enter to activate it, or Esc to cancel.`

You can also work from a shell in a Rex pane. `namedrop activate --input-json '{"confirmed":true}'` activates the session, and `namedrop name @build --type terminal` names the pane you are in. [Palette actions and keys](rex-reference.md#palette-actions-and-keys) lists every action.

## Start Claude Code and Codex in a Rex pane

1. Start each agent at the shell of its own Rex pane, as the pane's program.
	- Claude Code: start a new session, or run `/reload-plugins` in a running one.
	- Codex: start it as `codex --no-daemon`, or turn off its shared app-server as [Compatibility](compatibility.md#codex-0162-and-later-the-shared-app-server) describes. Namedrop cannot tell which pane a session of the shared app-server runs in, and Doctor reports `ND-SESSION-003` for it. In a new Codex session, approve Namedrop's hooks in `/hooks` if Codex asks.
2. Send each agent one prompt. Its hook then records which pane its session runs in. Codex runs its hooks only from its first prompt.
3. Name each agent's pane with **Name this pane**, and keep the pane type `agent`.
4. Type a mention at the start of Claude Code's prompt:

	```text
	@codex Reply with exactly the word pong.
	```

	Codex receives the message, and its answer comes back to Claude Code.

If an agent's pane gets no session, run **Namedrop Doctor** or `namedrop doctor`. `ND-SESSION-004` says why Namedrop refused to bind the session, and `ND-SESSION-003` reports a Codex shared app-server. `ND-SESSION-002` means that Namedrop has no session for the agent yet, because its hook has not run or is not installed. Run **Activate Namedrop** again, restart the agent, send it a prompt, and run Doctor again.

## Update Namedrop

A standalone Rex install checks for a new release once a day. When one is available, your agents show this at the start of a session, and Doctor shows it as `ND-UPDATE-001`:

```text
Namedrop <version> is available (you have <installed>). Update: download Namedrop <version> and run sh scripts/install.sh
```

To update:

1. Close any Namedrop screen that is open in Rex, such as the tour or Doctor.
2. Download and extract the new release, as in [Install Namedrop for Rex](#install-namedrop-for-rex).
3. Run `sh scripts/install.sh` from the new directory. It prints `Namedrop Rex install: update`, and keeps your names and messages.
4. Start new agent sessions.

To turn off the check, create `~/.config/namedrop/settings.json` with this content:

```json
{"check_for_updates": false}
```

On a Mac without the developer tools, the check does not run, because Apple's `/usr/bin/python3` and `/usr/bin/git` only offer to install them. Run `xcode-select --install` to turn the check on. [Updates](rex-reference.md#updates) gives the full rules.

If Herdr owns your Namedrop install, as after [Herdr first, then Rex](#herdr-first-then-rex), update it with **Update Namedrop** in Herdr. That updates Namedrop for both hosts.

## Use Herdr and Rex on one machine

A machine has one Namedrop: one state directory, one runtime, and one set of agent packages, which both hosts share. Which host you install first decides which install owns them.

### Run Herdr inside Rex

You can run Herdr as the program of a Rex pane and use Namedrop in Herdr's panes as usual. For a Herdr pane, Namedrop uses Herdr. See [Herdr inside Rex](compatibility.md#herdr-inside-rex).

### Herdr first, then Rex

Run `sh scripts/install.sh` as in [Install Namedrop for Rex](#install-namedrop-for-rex). It finds Herdr's install and prints `Namedrop Rex install: adopt` with Herdr's state directory. It installs the Rex module with Herdr's runtime, and publishes no runtime of its own. Herdr's updates then update Namedrop for both hosts.

<!-- TODO(round C): install.sh adopts a recorded Herdr state only while Herdr lists Namedrop. Confirm what it does once Herdr no longer lists Namedrop, and say so here. -->
The installer adopts Herdr's install only while Herdr lists Namedrop as an installed plugin.

The installer refuses in these cases and changes nothing:

- `Namedrop is already installed in Herdr (<dir>). Run Activate Namedrop in Herdr first, then run sh scripts/install.sh again.` Herdr's install has not written its install record yet. Run **Activate Namedrop** in a Herdr workspace, then run the installer again.
- `Namedrop is already installed in Herdr (<dir>). Update Namedrop in Herdr first, then run sh scripts/install.sh again.` Herdr runs a Namedrop release without Rex support. Run **Update Namedrop** in Herdr, then run the installer again.
- `Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.` A move from Rex to Herdr stopped part way. Run **Activate Namedrop** in Herdr.
- `ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in <state>. Run Activate Namedrop in Herdr to update them.` The state directory the installer would use belongs to Herdr. Use Herdr's install, as the message says.

### Rex first, then Herdr

When you activate Namedrop in Herdr, Herdr's install takes over the Rex install. It stops the Rex install's helpers, takes over the agent packages and the Rex module, and keeps the old state directory. Herdr's setup prints a notice:

```text
The names made on Rex stay in <old state> and were not carried over. In each Rex session, run Activate Namedrop again and name the panes again.
```

After the move:

1. In each Rex session, run **Activate Namedrop** again, and name the panes again.
2. Restart an agent that shows Hook failed or does not answer a mention. An agent turn that was running during the move can show Hook failed. Other agents run Herdr's hooks at their next turn.

Until each agent from before the move takes a turn or ends, Herdr's Doctor reports `ND-INSTALL-001`, "Agents on Rex have not run the new hooks yet." It lists each agent's pane. The old install's commands refuse with the short form of `ND-INSTALL-001`, which names Herdr's state directory. [ND-INSTALL-001](rex-reference.md#nd-install-001) gives both texts.

If you add a Herdr release without Rex support after a Rex install, Doctor on Rex reports `ND-INSTALL-003`. Update Namedrop in Herdr, then run **Activate Namedrop** in Herdr again.

## Remove Namedrop from Rex

1. Close every Namedrop screen that is open in Rex. Removal waits 60 seconds for an open screen, then fails with `Namedrop on Rex is busy`.
2. In a Rex pane, run:

	```sh
	namedrop install --remove --input-json '{"confirmed":true}'
	```

For a standalone Rex install, removal deletes the line it inserted in `init.lua`, the Rex module, the agent packages, the Claude Code permissions, the Codex rules and plugin entries, `~/.local/bin/namedrop`, and the install record. It prints `Namedrop was removed from Rex. Its state directory was preserved.` It keeps the state directory with your names and messages, the newest `init.lua` backup, and the `PATH` lines in your shell startup files. To install again, run `sh scripts/install.sh`. An install into the same state directory brings back your names.

If Herdr owns the install, removal deletes only the Rex module and the `init.lua` line, and prints `Removed only the Rex configuration. The shared packages and install record belong to another owner.` To remove Namedrop from Herdr too, follow [Uninstall](../README.md#uninstall).

## Mention an agent on another machine

In this section, A is your machine and B is the machine whose agent you mention. Set up B first, as [Set up the machine that receives](#set-up-the-machine-that-receives) describes.

1. Add B to A's Rex hosts. Pick a label of 1 to 64 ASCII letters, digits, `-`, and `_`. The label is the part of the address after the second `@`.

	```sh
	rex hosts add devbox <endpoint of B's Rex server>
	```

	Run `rex hosts add --help` for the endpoint forms. If you set up B with `rex server install ssh://`, B is already in your hosts.

2. Check that A can reach B:

	```sh
	rex hosts check devbox
	```

3. Make sure that `rex` is on the `PATH` of Namedrop's commands on A. The relay runs `rex` to reach B. A Rex pane has it. A shell outside Rex may not.
4. Ask a named agent on A to send the message. In Claude Code's prompt, type:

	```text
	Run this command: namedrop send --json --input-json '{"author_kind":"agent_session","request_id":"pong-1","source":"@codex@devbox Reply with exactly the word pong."}'
	```

	The command's `relayed` field is a list with one entry for each remote recipient. With `--json` the list sits under `data`, so the path is `data.relayed[0].statement`. Its `statement` says `@codex@devbox: queued for the Namedrop on devbox, which delivers it there.`

5. Wait for the reply. A collects B's replies every 30 seconds while a thread with B is open, which is for 24 hours after the last message that B accepted or the last reply that A collected. The reply arrives in Claude Code from `@codex@devbox`, in the thread of your message.

Send from an agent, not from a terminal, when you want to see the reply. A terminal can send the same message without `"author_kind":"agent_session"`. With it, the command fails, because only an agent's own session can send as an agent. Namedrop saves a reply to a terminal in its thread and does not print it in the terminal.

A remote address works only at the start of the `source` of `namedrop send`. Typed at the start of an agent's prompt, `@codex@devbox` is not a mention, and Namedrop sends nothing. The label must match a label that `rex hosts list` prints, letter for letter. Otherwise the send fails at once with `devbx is not in your Rex hosts (rex hosts list): devbox`, and nothing is queued.

When A's agent answers a reply from `@codex@devbox`, its answer goes back to `@codex` on B in the same thread. A message to a remote agent carries text only. It cannot attach context or tracked work.

## Set up the machine that receives

Do these steps on B, the machine whose agent A mentions. B runs Linux or macOS. Windows hosts are out of scope.

1. If B is a Linux machine that you reach with ssh and that has no Rex yet, set up its Rex server from A:

	```sh
	rex server install ssh://<user>@<host> --label devbox
	```

	The command installs Rex on B, sets up B's server to start on its own, and adds B to A's hosts as `devbox`. It supports only Linux machines. Run `rex server install --help` for its options. Namedrop has not been tested on a server that this command set up.

2. On B, install Namedrop as [Install Namedrop for Rex](#install-namedrop-for-rex) describes, with B's Rex server running. Run the installer on B itself, because it edits and reloads the configuration of the local Rex server only. Use the archive for B's platform. In an ssh shell on B, `rex` may not be on your `PATH`. Set `REX_BIN_DIR` to its directory, such as `~/.local/bin`.
3. Start the agents in panes of B's Rex server, then activate each session and name the agents. Use B's palette, or `namedrop` in a shell in one of B's panes.
4. Name the agent in only one of B's sessions. If the name is in two of them, B answers that the mention is ambiguous and names both sessions.
5. Keep B's bridge running. Rex starts it when B's server loads the Namedrop module, and any Namedrop command or hook in one of B's panes starts it too. While the bridge is stopped, A keeps the message and tries again. After 24 hours, A gives up and tells the sender `devbox was unreachable for 24 hours`.
6. Make sure that B's Rex server admits A's connection as a caller that B's Namedrop accepts. B takes a message only from a caller that Rex identifies as a Tailscale principal, or as a local peer with B's own user ID. Over `ssh://`, Rex identifies A as a local peer with the user ID that A logged in as, so log in as the user who runs B's Namedrop.
7. If B's agents must start new conversations with agents on A, add A to B's Rex hosts. B does not need A in its hosts to answer, because A collects B's replies.

B's Namedrop delivers a relayed mention as it delivers a local one. It types the message only when the agent's turn has ended, so a mention never interrupts B's agent mid-turn.

Unless Rex confirms which machine sent a message, the sender's host is the name that A gave itself. Then, before the first message of each thread, B's agent gets this notice: `[Namedrop: laptop is the name the sending machine gave itself, which Rex did not confirm.]` A Tailscale caller is named by its device instead, with no notice.
<!-- TODO(round C): the host notice's acknowledgement is not relayed. Confirm that an agent's answer to the notice stays on B, then state it here. -->
An agent's answer to this notice is not relayed to A.

## Let mentions from other machines reach a terminal

By default, B relays a mention only to an agent. A mention from A to a terminal on B fails, and the sender gets a notice in its thread: `Namedrop could not relay this message to @bterm@devbox: @bterm is a terminal on devbox, and devbox relays mentions only to agents.`

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

1. On A, run `namedrop doctor` in a pane of the session.
2. Find `ND-THREAD-001`, "A collaboration thread is waiting for approval." Its repair is a `thread continue` command with the thread's ID.
3. Run that command. It has this form:

	```sh
	namedrop thread continue --input-json '{"thread_id":"<thread ID>"}'
	```

Your own message in the thread resumes it too. The held replies then arrive in the order that B sent them. Each Continue allows 10 more replies, so a long run of held replies can pause the thread again. In that case, continue it again.

A holds at most 20 replies for one thread, and at most 100 for one host label on one local Rex server. B keeps the rest until A has room. Doctor's `ND-RELAY-003` tells you when a hold is full. A held reply that waits 7 days is let go, and the sender is told how many.

B's Namedrop applies the same limit to B's agents in B's thread. When B's limit pauses the thread, A keeps the message and tells the sender that a person on B must resume the thread there with a message or Continue.

## Check the relay

Run `namedrop doctor` on each machine. On A, `ND-RELAY-002` counts messages that did not reach another machine, and `ND-RELAY-003` reports replies that need a look. On B, `ND-RELAY-001` counts replies that expired before A acknowledged them. [Doctor on Rex](rex-reference.md#doctor-on-rex) gives the full texts.
