# Rex spike

This folder is step 1 of the [Rex host plan](../PLAN.md). It is throwaway code that answers whether Rex can carry Namedrop's mention loop. Nothing here ships, and nothing in `src/` uses it.

- `rexdrop.py` names Rex blocks, queues mentions, delivers a mention only when the recipient reports `idle` or `done`, and runs a bridge that reads `rex events`.
- `namedrop.lua` is a module that `init.lua` loads with `require("namedrop")`. It defines two palette actions. One names the current pane. The other opens a floating layer that lists the names.

## Result: go

The spike ran on 2026-11-11 with Rex `0.1.0-preview.14`, Claude Code 2.1.322 (Haiku, `--permission-mode default`), and Codex CLI 0.163.0. Everything ran on a separate Rex server with its own socket, state directory, and config, so the user's sessions were never touched.

| Question | Result |
|---|---|
| Can a process prove which block it runs in? | Yes. `name` with the real `REX_BLOCK` passed. With another block's ID in `REX_BLOCK`, it failed, because that block's child process was not an ancestor. |
| Does gated delivery reach an idle agent? | Yes, for both agents. Each replied to the pasted prompt. |
| Does a held mention drain without a manual command? | Yes. The bridge delivered a held mention to Claude Code 9 seconds after it was queued, and to Codex 69 seconds after. Each delivery followed a `program_status_changed` event to `idle` or `done`. |
| What happens to a prompt that arrives mid-turn? | The agents differ. Claude Code queued it and answered after the current turn finished. Codex treated it as steering. It dropped the 700-word story it was writing and answered only the new prompt. |
| What does Rex report during a permission prompt? | `state: blocked`, `kind: permission`, and a `msg` that names the command, for example `approve Bash: touch rexdrop-perm-test.txt`. The gate held the mention. After the prompt was cancelled, the bridge delivered it. |
| Does closing a named block remove the name? | Yes. The bridge saw `block_closed` and removed the name in the same second. |
| Do names survive a server restart? | Block IDs survive, but programs do not. After the restart, both blocks held a fresh login shell with new process IDs and no status records. The bridge exited when the server went away. |
| Can `init.lua` load a third-party module? | Yes. `require("namedrop")` loaded `namedrop.lua` from the folder beside `init.lua`. `rex actions` listed both actions after `rex config reload`. |
| Do the actions work? | Yes, both of them. `rex do namedrop_name alias=…` received the session ID and the block ID in `ctx`, and `rex.exec` ran `rexdrop.py`. `namedrop_list` opened a floating layer that ran a command. |
| Can Channels live peek stream output? | Not through the CLI. `rex block stream` prints `command "rex block stream" is not implemented`. The `data.connect` API method returns a ticket for a data connection, but the spike did not test it. |
| Can Namedrop find `rex` outside a Rex terminal? | Yes. The bundled binary at `~/Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex` ran with an empty environment and reached the default socket. |

## Findings that change the plan

- **Confirm delivery with an event, not a state.** `deliver` reports success when the state reads `working`. A forced send to an agent that was already working reported success at once. Real code must see a change to `working` that happens after the paste.
- **Never send to an agent that is not idle or done.** Codex abandons its current task for the new prompt. During a permission prompt, the Enter key would land on the dialog, where the highlighted choice is **Yes**. The spike did not press it, so this is an inference from the screen.
- **Rex does not always set `REX_SERVER`.** Blocks on the separate server had `REX_SESSION`, `REX_BLOCK`, and `REX_BIN_DIR`, but no `REX_SERVER`. A `rex` call from such a block reaches the default server. Namedrop must record the server address when it names a block and pass `-S` on every call.
- **The bridge must reconnect and reconcile.** When the server restarts, the event stream ends and each block holds a new program. Namedrop must check that the same agent session still runs in the block before it trusts a name again. The Herdr code already does this when a pane ID changes.
- **The palette is unverified.** The Rex app attaches only to the default server, so the spike could not show the actions in the palette. `rex actions` lists them, and the Rex docs say that custom actions appear in the palette.

## Run it

You need Rex, `python3`, Claude Code, and Codex. Run these commands from a Rex terminal.

1. Start a separate server. Keep the socket path short, because a Unix socket path has a length limit.

	```sh
	X=/tmp/rexdrop-spike; mkdir -p $X/state $X/cfg
	sed -e "s#__TOOL__#$PWD/rexdrop.py#" -e "s#__RD_SERVER__#unix://$X/s.sock#g" -e "s#__RD_STATE__#$X/rd#g" namedrop.lua > $X/cfg/namedrop.lua
	echo 'require("namedrop")' > $X/cfg/init.lua
	env -i HOME="$HOME" USER="$USER" SHELL=/bin/zsh PATH=/usr/bin:/bin rex server run --listen unix://$X/s.sock --state-dir $X/state --config $X/cfg/init.lua &
	export RD_SERVER=unix://$X/s.sock RD_STATE=$X/rd
	```

	The `env -i` keeps this Claude Code session's variables out of the agents that the server starts.

2. Start one agent of each kind in a folder that both agents trust, and accept any prompts in the panes.

	```sh
	S=$(rex -S $RD_SERVER new spike -c "$PWD" --json -- zsh -lc 'claude --permission-mode default' | jq -r .session_id)
	rex -S $RD_SERVER window new -s $S Codex -c "$PWD" --json -- zsh -lc codex
	rex -S $RD_SERVER block ls -s $S --json
	```

3. Name the two blocks, start the bridge, and send a mention.

	```sh
	python3 rexdrop.py name claude --session $S --block <claude block>
	python3 rexdrop.py name codex --session $S --block <codex block>
	python3 rexdrop.py bridge &
	python3 rexdrop.py send codex "Reply with exactly: pong" --from claude
	```

	The last command prints `"event": "delivered"` when Codex is idle, and `"event": "held"` when it is busy. The bridge prints the delivery of a held mention when Codex finishes.

4. Stop the bridge, the session, and the server.

	```sh
	kill %2 %1
	rex -S $RD_SERVER kill $S
	```
