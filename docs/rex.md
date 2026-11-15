# Run Namedrop on Rex

Rex support is experimental. It lives on the `experimental` branch and is not in any release. This guide sets up Namedrop in a [Rex](https://www.superlogical.com/rex/docs) session and connects it to agents on a second machine. For Doctor's Rex diagnostics, the relay's limits, and what does not work yet, see [Namedrop on Rex reference](rex-reference.md). For the Rex versions that Namedrop accepts, see [Compatibility](compatibility.md#rex-experimental).

On Rex, Namedrop names blocks, delivers mentions and replies between agents in one Rex session, and keeps names current through its bridge, a background process that reads Rex's events. Its relay sends a mention to an agent on another machine that runs its own Rex server and its own Namedrop.

Rex has no installer, palette actions, or key bindings for Namedrop yet. They are planned. Until then you work from a shell in a Rex block.

## Set up Namedrop in a Rex session

Do these steps on each machine that runs agents.

1. Build Namedrop from the `experimental` branch:

	```sh
	git clone https://github.com/ericbust/namedrop
	cd namedrop
	git switch experimental
	cargo build --release --locked
	```

2. Open a shell in a block of the Rex session where your agents will run. Run Doctor from the checkout:

	```sh
	./target/release/namedrop doctor
	```

	Doctor reports `ND-SETUP-001`, "Namedrop is not active in this space." A Rex session is one space.

3. Activate Namedrop in the session:

	```sh
	./target/release/namedrop activate --input-json '{"confirmed":true}'
	```

	Activation installs the Claude Code and Codex packages and adds the `namedrop` command to `~/.local/bin`. Any Namedrop command or hook in a Rex block starts the bridge when it is not running. Activation prints the state directory that Namedrop uses on the `State` line, under `Details`. Note it: you need it to turn on [terminals for other machines](#let-mentions-from-other-machines-reach-a-terminal). [Files in the state directory](rex-reference.md#files-in-the-state-directory) says how Namedrop picks the directory.

4. Do the agent steps that activation prints. For Claude Code, start a new session or run `/reload-plugins`. For Codex, run the two `codex plugin` commands it prints, then start a new Codex session and approve Namedrop's hooks in `/hooks`.
5. Open a new block, so that `namedrop` is on your `PATH`.
6. Start each agent at the shell of its own block, as the block's program. Start Codex as `codex --no-daemon`, or turn off its shared app-server as [Compatibility](compatibility.md#codex-0162-and-later-the-shared-app-server) describes. Namedrop cannot tell which block a session of the shared app-server runs in.
7. Send each agent one prompt. Its hook then records which block its session runs in. Codex runs its hooks only from its first prompt.
8. Find the block IDs. In a shell block of the session, run:

	```sh
	rex block ls --short=false
	```

	A full block ID looks like `block:01a1dc74-dade-78fc-aa4a-fdedd2b0606f`.

9. Name each agent's block from that shell block:

	```sh
	namedrop name @claude --pane <Claude Code's block ID>
	namedrop name @codex --pane <Codex's block ID>
	```

	To name the shell block you are in, run `namedrop name @shell --type terminal`.

10. Type a mention at the start of Claude Code's prompt:

	```text
	@codex Reply with exactly the word pong.
	```

	Claude Code shows "Collaboration message sent." Codex receives the message, and its answer comes back to Claude Code.

If an agent's block gets no session, run `namedrop doctor`. `ND-SESSION-004` says why Namedrop refused to bind the session, and `ND-SESSION-003` reports a Codex shared app-server. `ND-SESSION-002` means that Namedrop has no session for the agent yet, because its hook has not run or is not installed. On Rex its text still names Herdr. Finish the agent steps of activation, send the agent a prompt, and run Doctor again.

## Mention an agent on another machine

In this section, A is your machine and B is the machine whose agent you mention. Set up B first, as [Set up the machine that receives](#set-up-the-machine-that-receives) describes.

1. Add B to A's Rex hosts. Pick a label of 1 to 64 ASCII letters, digits, `-`, and `_`. The label is the part of the address after the second `@`.

	```sh
	rex hosts add devbox <endpoint of B's Rex server>
	```

	Run `rex hosts add --help` for the endpoint forms.

2. Check that the label is listed:

	```sh
	rex hosts list
	```

3. Make sure that the `rex` command is on the `PATH` of Namedrop's commands on A. The relay runs `rex` to reach B.
4. Ask a named agent on A to send the message. In Claude Code's prompt, type:

	```text
	Run this command: namedrop send --json --input-json '{"author_kind":"agent_session","request_id":"pong-1","source":"@codex@devbox Reply with exactly the word pong."}'
	```

	The command's `relayed` field says `@codex@devbox: queued for the Namedrop on devbox, which delivers it there.`

5. Wait for the reply. A collects B's replies every 30 seconds while a thread with B is open, which is for 24 hours after the last message that B accepted or the last reply that A collected. The reply arrives in Claude Code from `@codex@devbox`, in the thread of your message.

Send from an agent, not from a terminal, when you want to see the reply. A terminal can send the same command, but Namedrop saves a reply to a terminal in its thread and does not print it in the terminal.

A remote address works only at the start of the `source` of `namedrop send`. Typed at the start of an agent's prompt, `@codex@devbox` is not a mention, and Namedrop sends nothing. The label must match a label that `rex hosts list` prints, letter for letter. Otherwise the send fails at once with `devbx is not in your Rex hosts (rex hosts list): devbox`, and nothing is queued.

When A's agent answers a reply from `@codex@devbox`, its answer goes back to `@codex` on B in the same thread. A message to a remote agent carries text only. It cannot attach context or tracked work.

## Set up the machine that receives

Do these steps on B, the machine whose agent A mentions.

1. Set up Namedrop in the Rex session where the agent runs, as [Set up Namedrop in a Rex session](#set-up-namedrop-in-a-rex-session) describes.
2. Name the agent in only one of B's sessions. If the name is in two of them, B answers that the mention is ambiguous and names both sessions.
3. Keep B's bridge running. Any Namedrop command or hook in a Rex block on B starts it, so `namedrop doctor` in a B block is enough. While the bridge is stopped, A keeps the message and tries again. After 24 hours, A gives up and tells the sender `devbox was unreachable for 24 hours`.
4. Make sure that B's Rex server admits A's connection. B's Namedrop takes a message only from a caller that Rex identifies as a Tailscale principal, or as a local peer with B's own user ID.
5. If B's agents must start new conversations with agents on A, add A to B's Rex hosts. B does not need A in its hosts to answer, because A collects B's replies.

B's Namedrop delivers a relayed mention as it delivers a local one. It types the message only when the agent's turn has ended, so a mention never interrupts B's agent mid-turn.

## Let mentions from other machines reach a terminal

By default, B relays a mention only to an agent. A mention from A to a terminal on B fails, and the sender gets a notice in its thread: `Namedrop could not relay this message to @bterm@devbox: @bterm is a terminal on devbox, and devbox relays mentions only to agents.`

A terminal runs each new message as a shell command. So a relayed mention to a terminal on B runs a command on B, under your account, that someone wrote on A. Turn this on only if you want the people and agents on A to run commands on B.

To let relayed mentions reach B's terminals, create the file `relay/allow-terminals` in B's state directory. Use the directory on the `State` line that activation printed on B:

```sh
state='<the directory on the State line>'
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

1. On A, run `namedrop doctor` in a block of the session.
2. Find `ND-THREAD-001`, "A collaboration thread is waiting for approval." Its repair is a `thread continue` command with the thread's ID.
3. Run that command. It has this form:

	```sh
	namedrop thread continue --input-json '{"thread_id":"<thread ID>"}'
	```

Your own message in the thread resumes it too. The held replies then arrive in the order that B sent them. Each Continue allows 10 more replies, so a long run of held replies can pause the thread again. In that case, continue it again.

A holds at most 20 replies for one thread, and at most 100 for one host label on one local Rex server. B keeps the rest until A has room. Doctor's `ND-RELAY-003` tells you when a hold is full. A held reply that waits 7 days is let go, and the sender is told how many.

B's Namedrop applies the same limit to B's agents in B's thread. If B's agents stop replying, a person on B may have to continue the thread on B.

## Check the relay

Run `namedrop doctor` on each machine. On A, `ND-RELAY-002` counts messages that did not reach another machine, and `ND-RELAY-003` reports replies that need a look. On B, `ND-RELAY-001` counts replies that expired before A collected them. [Doctor on Rex](rex-reference.md#doctor-on-rex) gives the full texts.
