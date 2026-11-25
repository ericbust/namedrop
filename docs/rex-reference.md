# Namedrop on Rex reference

This page describes Namedrop's experimental Rex support. To set it up, see [Run Namedrop on Rex](rex.md). For the Rex versions that Namedrop accepts, see [Compatibility](compatibility.md#rex-experimental).

## Terms

| Term | Meaning |
|---|---|
| Rex session | One Rex session, which Namedrop activates as one space. Names are per session |
| Pane | A Rex block. Its ID has the form `block:<uuid>`. Namedrop's texts call it a Rex pane |
| Screen | A Namedrop screen, such as Name this pane or Doctor. It opens in a floating Rex layer |
| Standalone install | A Namedrop install that `scripts/install.sh` made, with no Herdr. Its `runtime/owner` file says `standalone` |
| Herdr install | A Namedrop install that Herdr made. Its `runtime/owner` file says `herdr` |
| Module | `namedrop.lua`, the Lua file that defines Namedrop's palette actions and keys. `init.lua` loads it |
| Bridge | `namedrop rex bridge`, one background process for each Rex server. It reads the server's events, retires the names of closed panes, delivers queued mentions, starts the update check, and answers the relay's calls |
| A | The machine that sends a mention to another machine |
| B | The machine whose agent receives that mention |
| Relay worker | `namedrop relay worker`, one background process on A for each host label. It sends A's queued messages to B and collects B's replies |

## install.sh

```text
sh scripts/install.sh [--yes]
```

| Option | Effect |
|---|---|
| `--yes` | Answers yes to `Install Namedrop for Rex? [y/N]` |
| `-h`, `--help` | Prints the usage and exits 0 |

Without `--yes`, any answer other than `y` or `yes` cancels with `Installation cancelled; no files changed.` A closed standard input answers no.

### What install.sh checks

`install.sh` checks these in order. It changes nothing before you confirm.

1. The platform, as `uname` reports it: macOS on Apple silicon, or Linux on x86_64 or aarch64. `install.sh` does not check the C library. `scripts/build.sh` checks for glibc.
2. Python 3: it runs `python3`, because on a Mac without the developer tools `/usr/bin/python3` exists but fails.
3. Rex: `rex` on `PATH`, else `$REX_BIN_DIR/rex`.
4. The install record, `~/.config/namedrop/state-directory`. The record counts only when it names an absolute path whose `runtime/namedrop` is an executable file. The record's path follows `HOME` and ignores `XDG_CONFIG_HOME`.
5. The state directory: the recorded one, else `$NAMEDROP_STATE_DIR`, else `$XDG_STATE_HOME/namedrop`, else `~/.local/state/namedrop`. It must be an absolute path.
6. The owner of that state directory, and any marker that a removal or a move to Herdr left there.
7. With no record, Herdr's Namedrop state: `$XDG_STATE_HOME/herdr/plugins/namedrop` if `XDG_STATE_HOME` is an absolute path, else `~/.local/state/herdr/plugins/namedrop`. It counts only while Herdr's plugin list names Namedrop.
8. With a record that names a Herdr state, Herdr's plugin list. The state is adopted only while the list names Namedrop.
9. The Rex server, which the package's own `namedrop --install-preview` finds. That is the socket that `REX_SERVER` names, else Rex's default socket. On macOS, the default is `~/Library/Application Support/rex/server.sock`. On Linux, it is `$XDG_RUNTIME_DIR/rex/server.sock`. The preview writes nothing.

`install.sh` reads Herdr's plugin list where Herdr reads it. With `XDG_CONFIG_HOME` set, even to an empty or relative value, the list is `$XDG_CONFIG_HOME/herdr/plugins.json`. `install.sh` resolves an empty or relative value from the directory you run it in. With `XDG_CONFIG_HOME` unset, the list is `~/.config/herdr/plugins.json`. A missing list names no plugin. Under an empty or relative `XDG_CONFIG_HOME`, a missing list stops the run as an unreadable one does, because Herdr resolves that directory from its own working directory.

| Decision | When | What it does |
|---|---|---|
| `new` | No record, and the state directory has no owner, a `standalone` owner, or a removal marker | Publishes the runtime into the state directory, then runs `namedrop install` |
| `update` | The record names a `standalone` state | Publishes the new runtime into that state, migrates its store when the schema changed, then runs `namedrop install` |
| `adopt` | The record names a Herdr state whose runtime supports Rex, and Herdr's plugin list names Namedrop | Runs `namedrop install` with Herdr's runtime, and publishes no runtime. Herdr's updates keep updating it |

### install.sh messages

Each message below ends the run. A message that the table marks "stopped" prints after `Namedrop Rex installation stopped: `. The table leaves out other stopped messages, which name a problem with `HOME`, with an owner file, or with a marker in the state directory.

| Message | Exit | Cause |
|---|---|---|
| `Unknown argument: <argument>` | 64 | An argument other than `--yes`, `-h`, or `--help` |
| `Namedrop supports macOS on Apple silicon and GNU/Linux on x86_64 or aarch64; this machine reports <platform>.` | 69 | Another platform, or an Intel-emulated terminal on a Mac |
| `Namedrop needs Python 3. On macOS, install the Command Line Tools with xcode-select --install, then run sh scripts/install.sh again.` | 69 | `python3` does not run |
| `Namedrop needs Rex. Put rex on PATH, or set REX_BIN_DIR to the directory containing rex, then run sh scripts/install.sh again.` | 69 | No `rex` found |
| `Namedrop found no running Rex server: <error>. Start Rex, or set REX_SERVER to its Unix socket, then run sh scripts/install.sh again.` | 69 | No server answers on the socket |
| `Namedrop cannot tell where Rex's server is. Start Rex, or set REX_SERVER to its Unix socket, then run sh scripts/install.sh again.` | 69 | `REX_SERVER` names no Unix socket. Or it is unset, and on Linux `XDG_RUNTIME_DIR` is unset too, as in some ssh shells |
| `No built Namedrop executable was found. Run sh scripts/build.sh, or unpack the release archive again.` | 2, stopped | A checkout with no `bin/namedrop` |
| `Namedrop is already installed in Herdr (<dir>). Run Activate Namedrop in Herdr first, then run sh scripts/install.sh again.` | 2, stopped | Herdr lists Namedrop and its runtime supports Rex, but it has written no install record |
| `Namedrop is already installed in Herdr (<dir>). Update Namedrop in Herdr first, then run sh scripts/install.sh again.` | 2, stopped | Herdr lists Namedrop, and its runtime has no Rex support |
| `Namedrop's install record names Herdr's Namedrop state (<dir>), but Herdr's plugin list in <file> does not name Namedrop. If Herdr uses another config directory, run sh scripts/install.sh again with Herdr's XDG_CONFIG_HOME. Otherwise install Namedrop in Herdr again, or move that state directory away, then run sh scripts/install.sh again.` | 2, stopped | The record names a Herdr state, and the plugin list that `install.sh` read does not name Namedrop. A debug Herdr, or a Herdr whose `XDG_CONFIG_HOME` differs from your shell's, keeps its list elsewhere. Once `<dir>` is moved away, the record no longer counts, and the next run is a new install |
| `Update Namedrop in Herdr first, then run sh scripts/install.sh again.` | 2, stopped | The record names a Herdr state that Herdr lists, and its runtime has no Rex support |
| `Namedrop cannot read Herdr's plugin list in <file>, so it cannot tell whether Namedrop is still installed in Herdr (<dir>). If Herdr uses another config directory, run sh scripts/install.sh again with Herdr's XDG_CONFIG_HOME. Otherwise, if Namedrop is not installed in Herdr, move that state directory away, then run sh scripts/install.sh again.` | 2, stopped | Herdr's `plugins.json` cannot be read or parsed, or it is missing under an empty or relative `XDG_CONFIG_HOME`. `<file>` is an absolute path |
| `Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.` | 2, stopped | A move to Herdr stopped part way |
| `ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in <state>. Run Activate Namedrop in Herdr to update them.` | 2, stopped | The state directory that `install.sh` would use belongs to Herdr |
| `Installation cancelled; no files changed.` | 0 | You answered no |
| `Installation cancelled; no files changed.` | 130 | You pressed Ctrl-C before you confirmed |
| `Namedrop Rex installation interrupted. Run sh scripts/install.sh again to finish it.` | 130 | Ctrl-C after you confirmed |

When `XDG_CONFIG_HOME` is relative, the two messages about Herdr's plugin list add, after their first sentence: `XDG_CONFIG_HOME is relative, so Namedrop read that list from this working directory, which may not be Herdr's.`

A failure inside the install prints the install's own message and exits with its code. In an update, the message says whether the previous runtime stays selected. An update that changes the store's schema waits 20 seconds for an open Namedrop screen, then stops before it switches runtimes and asks you to close the screen, such as the tour.

## What the install writes

`install.sh` lists each path before it asks. A confirmed install writes these.

| What | Where |
|---|---|
| Runtime and store | `<state>/runtime/`: `releases/<sha256>/`, the `current` link, the launcher `namedrop`, `owner`, `source.json`, and migration records. The install also creates the store in `<state>`, or migrates it. An adoption writes none of these |
| Rex module | `namedrop.lua`, in the folder of the `init.lua` that the Rex server reads. Its first line is `-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.` |
| Rex config line | `pcall(require, "namedrop");` as the first line of `init.lua`. When `init.lua` already exists, the install first copies it to `init.lua.namedrop-<UTC time>.bak` beside it. Only the newest backup is kept |
| Shell command | `~/.local/bin/namedrop` |
| PATH lines | A `# Namedrop CLI` block in `.zshrc` under zsh, or in `.bashrc` and a login file under bash. Other shells get none |
| Codex package | `~/.agents/skills/namedrop` |
| Claude Code package | `~/.claude/skills/namedrop` |
| Claude Code permissions | `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json` |
| Codex rules, sandbox root and plugin | `$CODEX_HOME`, else `~/.codex`. With `codex` on `PATH`, the install also runs `codex plugin marketplace add` and `codex plugin add namedrop@namedrop` |
| Install records | `~/.config/namedrop/`: `state-directory` names the state directory, and `rex.json` names the Rex server, `init.lua`, the module, the launcher, and the state |

The install edits `init.lua` only when the file is a regular file or missing. It counts the line as present only as a whole line of its own. Before it edits, it reloads the unedited file. If that reload fails, it edits nothing and stops. If the reload after the edit fails, it restores `init.lua` from the backup. When it cannot edit the file, it prints the line, the reason, and the file to put it in.

`~/.config/namedrop/` also holds `settings.json`, if you create it, and `packages.lock`. These paths follow `HOME` and ignore `XDG_CONFIG_HOME`.

## namedrop install

`install.sh` runs `namedrop install`. You can run it from a shell in a Rex pane.

| Command | Effect |
|---|---|
| `namedrop install --input-json '{"confirmed":true}'` | Installs or refreshes the module, the `init.lua` line, and the agent packages for this runtime. Without confirmation it lists what it would write and changes nothing |
| `namedrop install --print-config --input-json '{"confirmed":true}'` | Installs as above, but prints the `init.lua` line and where it goes instead of editing `init.lua` |
| `namedrop install --remove --input-json '{"confirmed":true}'` | Removes Namedrop from Rex. Without confirmation it says what removal deletes and changes nothing |

Removal deletes the first line of `init.lua` only when it is the line that the install inserted. It deletes `namedrop.lua` only when its first line is the managed line, then deletes `rex.json`. For a standalone install, it also stops the install's background processes, such as its bridge, and removes the agent packages, the permissions, the Codex rules and plugin entries, `~/.local/bin/namedrop`, and the install record. It never deletes the state directory or the `PATH` lines. When it edits `init.lua`, it backs the file up as the install does, and keeps only that newest backup.

| Removal result | Meaning |
|---|---|
| `Namedrop was removed from Rex. Its state directory was preserved.` | A standalone install was removed |
| `Removed only the Rex configuration. The shared packages and install record belong to another owner.` | A Herdr install, or another install, owns the packages |
| `Namedrop on Rex is busy. Close any Namedrop screen that is open in Rex, such as the tour, then run namedrop install --remove again.` | A Namedrop screen stayed open for 60 seconds. Nothing changed |
| `Namedrop was installed again. Removal stopped.` | An install ran while removal ran |

After a standalone removal, the old runtime's commands print `Namedrop was removed from Rex. Run sh scripts/install.sh to install it again.` and exit 69.

## Palette actions and keys

The module defines these actions in the `Namedrop` category of the Rex command palette. Each one except About Namedrop runs `namedrop rex action <name>` for the pane you run it from. About Namedrop answers from the module itself.

| Title | Rex action | Key | Effect |
|---|---|---|---|
| Activate Namedrop | `namedrop_activate` | None | Activates Namedrop in this Rex session. Opens the tour if an agent is present and this machine has not shown the tour yet, else opens Doctor |
| Name this pane | `namedrop_name` | `alt+shift+cmd+i` | Opens the naming screen. In a session that is not active, it offers to activate the session first |
| Manage Namedrop names and agents | `namedrop_manage` | `alt+shift+cmd+m` | Opens the screen that manages names and agents |
| Namedrop Doctor | `namedrop_doctor` | `alt+shift+cmd+h` | Opens Doctor's report for this session |
| Agent status | `namedrop_status` | `alt+shift+cmd+s` | Opens Agent status for this pane |
| Namedrop tour | `namedrop_tour` | `alt+shift+cmd+o` | Opens the tour |
| Open Namedrop Channels | `namedrop_channels` | None | Opens Channels in your browser. See [Limits](#limits-of-rex-support) |
| Pause Namedrop | `namedrop_pause` | None | Pauses all collaboration in this Rex session |
| Resume Namedrop | `namedrop_resume` | None | Resumes collaboration in this Rex session |
| Deactivate Namedrop | `namedrop_deactivate` | None | Deactivates Namedrop in this Rex session and keeps its data |
| About Namedrop | `namedrop_about` | None | Returns which module Rex loaded, its launcher and state, and the server's `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, and `REX_SERVER`. The installer and Doctor use it to confirm the module |

On a Mac, `alt+shift+cmd+i` is Option-Shift-Command-I.

The module binds a key only when no binding holds that key when Rex loads the module. A key that `init.lua` binds after the Namedrop line wins over Namedrop's. The install report and Doctor's `ND-REX-001` list each key as in effect, shadowed, or not bound to Namedrop.

To run an action without the palette, run `rex do namedrop_<name>` in a Rex pane, for example `rex do namedrop_doctor`.

| Action error | Cause |
|---|---|
| `Run this from a Rex pane` | The action has no session or pane to act on |
| `Namedrop's launcher <path> is missing. Run sh scripts/install.sh again.` | The launcher that the module names is gone |
| `<title> did not finish within 25 s, so Rex stopped it. If Namedrop is updating, try again when the update is done.` | The action took longer than 25 seconds |
| `Namedrop is active, but part of its setup needs attention. Namedrop Doctor lists the repair.` | Activate Namedrop activated the session, and an agent package is missing or reports an error |

When Rex loads the module, it also starts `namedrop rex ensure --from-server`, which starts the bridge for that server.

## Updates

| What | Value |
|---|---|
| Who checks | The bridge of a standalone install, when it connects to its server and every hour while it stays connected. A Herdr install keeps Herdr's own checks |
| How often a check runs | At most once a day. `update-check.json` in the state directory records the last check |
| Source | `runtime/source.json`. An archive install checks `ericbust/namedrop`. A checkout checks its `origin`, if that is a GitHub repository, and otherwise is not checked |
| How it checks | `git ls-remote --tags` lists the source's `v*` tags |
| Releases offered | Newer than the installed version, at or above the first release that ships `install.sh`, and on the install's channel. A stable install is offered stable releases. A pre-release is offered the later pre-releases of its own version and newer stable releases |
| Settings file | `~/.config/namedrop/settings.json` for a standalone install. A Herdr install reads `$XDG_CONFIG_HOME/herdr/plugins/config/namedrop/settings.json`, else `~/.config/herdr/plugins/config/namedrop/settings.json` |
| Setting | `check_for_updates`, `true` or `false`. A missing file means `true`. Any other key makes the file invalid, and Doctor reports `ND-UPDATE-002` |
| Agent notice | At the start of a session, at most once a day for each version: `Namedrop <version> is available (you have <installed>). Update: download Namedrop <version> and run sh scripts/install.sh` |
| macOS without the developer tools | The bridge starts no check when the first `python3` on its `PATH` is `/usr/bin/python3`, by name or through a link, and `xcode-select -p` fails. It runs `xcode-select -p` only when the setting and the daily limit allow a check. A check that runs anyway records `skipped` with `developer_tools_missing`, and runs no `git` |

## One machine with Herdr and Rex

One machine has one set of agent packages: `~/.claude/skills/namedrop`, `~/.agents/skills/namedrop`, `~/.local/bin/namedrop`, the install record, the Codex rules, and the Codex plugin. The install that the record names owns them. Every writer takes the per-user lock `~/.config/namedrop/packages.lock` first.

| Order | Result |
|---|---|
| Herdr first, then Rex | `install.sh` adopts Herdr's install. The module names Herdr's launcher and state |
| Rex first, then Herdr | Herdr's activation retires the standalone state and takes over the packages, the record, and the module. The names made on Rex stay in the old state |
| Rex first, then a Herdr release without Rex support | That release takes the packages and leaves the module naming the standalone state. Doctor on Rex reports `ND-INSTALL-003` |

The retired state keeps `runtime/retired.json`, with `reason` `herdr_handoff` and the new state as `successor`. From then on, its hooks exit with no output, its background processes exit, and its commands print the short form of `ND-INSTALL-001` and exit 69. `install.sh` refuses that state.

## Remote addresses

A remote address is `@<name>@<label>`.

- `<name>` is a name in a Rex session on B, at most 64 bytes.
- `<label>` is a label that `rex hosts list` prints on A, matched letter for letter. It is 1 to 64 ASCII letters, digits, `-`, and `_`.
- Only the leading recipients of `namedrop send` can be remote. In an agent's prompt, and in a message body, `@name@label` is not a remote address.
- A send to a remote address carries text only. It refuses `context`, `request_context`, tracked work, and `in_reply_to`, with `a message to an agent on another host is a plain send: it carries no context and no tracked work`.
- A send to a label that `rex hosts list` does not print fails at once with `<label> is not in your Rex hosts (rex hosts list): <labels>`, and Namedrop queues nothing.
- A relayed message shows its sender as `@<name>@<host>`. When Rex identifies A as a Tailscale principal, `<host>` is A's Tailscale device name. Otherwise it is the name that A's Namedrop gives its machine, which is A's host name up to its first dot. In that case B's agent gets `[Namedrop: <host> is the name the sending machine gave itself, which Rex did not confirm.]` before the first message of each thread.
- An answer to that notice reaches A as an answer to the message only when B had tried to deliver the message to the answering agent before the agent answered. Any other answer to the notice is an acknowledgement. B neither keeps it for A nor counts it, so A never gets it.

## Relay timings and sizes

| What | Value |
|---|---|
| How often A collects replies while a thread with B is open | Every 30 seconds, and with each message A sends |
| How long a thread stays open | 24 hours after the last message B accepted or the last reply A collected |
| How long A tries to deliver a message | 24 hours. Then it fails with `<label> was unreachable for 24 hours` |
| Wait between tries after a fault | 1, 2, 5, and 10 seconds, then 30 seconds for each later try |
| How long one `rex` call may take | 15 seconds. A message whose call timed out is sent again with the same ID, and B accepts it once |
| How often A's bridge restarts a stopped relay worker | Every 10 seconds, for each label with a message queued or a thread open |
| Largest message A sends | A request of 60 KiB. A larger send fails at once with `the message to @<name>@<label> is too large to relay: its request is <n> bytes, and the limit is 61440` |
| Largest reply text | 60 KiB. A longer reply is cut and ends with `[Namedrop cut this message to relay it.]` |
| Largest reason A keeps from B | 1 KiB |
| How long B keeps a reply for A | 7 days |
| Most replies B keeps for one A host | 10,000. B keeps no reply past that, counts it in its own `ND-RELAY-001`, and tells A |
| Replies the Automatic Reply Limit allows before it pauses a thread | 10 with no message from a person. Each Continue allows 10 more |
| Most replies A holds for paused threads | 20 for one thread, and 100 for one host label on one local Rex server |
| How long A holds a reply | 7 days. Then A lets it go and tells the sender how many it let go |

## What a sender sees when a relayed message fails

A tells the sender of a failed message once, in its thread, as a message from the remote agent: `Namedrop could not relay this message to @<name>@<label>: <reason>.` In the reasons below, `<B>` is B's host name up to its first dot.

| Reason | Cause |
|---|---|
| `no @<name> on <B>` | No session on B has that name |
| `@<name> is in a paused space on <B>` | Namedrop is paused in that session on B |
| `@<name> is a terminal on <B>, and <B> relays mentions only to agents` | The name is a terminal, and B has no `relay/allow-terminals` |
| `@<name> is in more than one session on <B>: <sessions>` | The name is in two or more of B's sessions |
| ``the relay does not accept the caller kind `<kind>` here`` | B's Rex server admitted the caller as something other than a Tailscale principal or a local peer with B's user ID, such as `anonymous` |
| `the relay does not accept a caller with no principal` | B's Rex server named no principal for the caller |
| `<label> now answers as another Rex server` | The label reaches a different Rex server than when A first sent the message |
| `<label>'s Rex server answers as the one <other> reaches, which your Rex hosts list gives other addresses` | Two labels reach one server, and `rex hosts list --json` gives them different endpoints |
| `<label> runs a Namedrop without this relay` | B's Namedrop has no relay, or a different one |
| `<label> is no longer in your Rex hosts` | Someone removed the label after the send |
| `<label> keeps no more replies for this machine until this machine collects them` | B keeps 10,000 replies for A. Each message that A had not sent yet fails |
| `<label> was unreachable for 24 hours` | B did not answer for 24 hours |
| `the Automatic Reply Limit kept its thread on <label> paused for 24 hours` | B's Automatic Reply Limit paused the thread, and the message still waited 24 hours after A queued it |
| `the message is too large to relay` | B's Rex server refused the call as too large |

While B's bridge is stopped or B cannot be reached, A keeps the message and tries again. A refusal is stored with the message, so a later change on B, such as a new `relay/allow-terminals`, does not change the answer to that message.

A also tells the sender in the thread when:

- B's Automatic Reply Limit paused the thread on B: `The Automatic Reply Limit paused this thread on <label>, so <label> takes no more messages in it for now. Namedrop keeps this message for @<name>@<label> and sends it once a person on <label> resumes the thread there with a message or Continue.`
- A's own limit holds B's replies: `The Automatic Reply Limit paused this thread, so Namedrop is holding the replies from @<name>@<label>. A person's message in this thread, or Continue, resumes it, and the held replies follow.`
- B let go of replies to the sender's message: `The machine of @<name>@<label> let go of <replies> ...`. This notice comes at most once for each B agent and message, and at most 4 times an hour for one server.

## Files in the state directory

On Rex, Namedrop commands use the first of these state directories that they find:

1. The directory that `--plugin-state` names. The `namedrop` command in `~/.local/bin` passes the directory it was installed with.
2. `$NAMEDROP_STATE_DIR`, if it is set and not empty. The module passes it to each action.
3. The directory in the install record, `~/.config/namedrop/state-directory`. A record that is missing, unreadable, or not an absolute path does not count.
4. `$XDG_STATE_HOME/namedrop` if `XDG_STATE_HOME` is an absolute path, or else `~/.local/state/namedrop`.

`install.sh` prints the directory it uses on its `State directory:` line. Use that directory, rather than working it out from this list.

| File | Effect |
|---|---|
| `relay/allow-terminals` | On B, lets a relayed mention reach a terminal, which runs it as a shell command. Off when the file is missing |
| `relay.sqlite3` | The relay's own database: messages to and from other machines, held replies, and what Doctor reports |
| `relay.sqlite3.unreleased-<ms>` | A relay database from an unreleased build that Namedrop set aside. `ND-RELAY-004` names it |
| `runtime/owner` | `standalone` or `herdr` |
| `runtime/source.json` | The installed version, and `archive` or `checkout` with its origin. The update check reads it |
| `runtime/retired.json` | Written when a removal or a move to Herdr retired the state |
| `update-check.json` | The last update check |

## Doctor on Rex

Run `namedrop doctor`, or **Namedrop Doctor** from the palette. These diagnostics apply to Rex. Each one leaves the repair to you.

### ND-SETUP-001

| Field | Text |
|---|---|
| Severity | `blocked` |
| Summary | "Namedrop is not active in this Rex session." |
| Explanation | "Messages will wait until Namedrop is activated." |
| Repair | "Activate Namedrop?" |

### ND-REX-001

Doctor reports `ND-REX-001` when `~/.config/namedrop/rex.json` names a module.

| Field | Text |
|---|---|
| Severity | `info` when confirmed, else `notice` |
| Summary | "The Rex module is confirmed." or "The Rex module is not confirmed." |
| Explanation | "Registered module: `<path>`. Loaded module: `<path>`, launcher `<path>`, state `<path>`. Server config: `<path>`. Server environment: `<variables>`. Keys: `<keys>`." With no answer from the module, "No module answered namedrop_about." takes the place of the loaded module. A removed line adds "The Namedrop line was removed. To restore it, add pcall(require, "namedrop"); as its own line in `<path>`." |
| Repair, confirmed | "Keys bound later in init.lua override the Namedrop keys." |
| Repair, older module still loaded | "Reload Rex's configuration with rex config reload. Then check Namedrop Doctor again." |
| Repair, otherwise | "Run sh scripts/install.sh, then check Namedrop Doctor again." |

### ND-INSTALL-001

Herdr's Doctor reports the warning form after a move from Rex to Herdr. It clears as each agent from before the move takes a turn or ends, or when the old state directory or `rex.json` is deleted.

| Field | Text |
|---|---|
| Severity | `notice` |
| Summary | "Agents on Rex have not run the new hooks yet." |
| Explanation | "Namedrop on Rex moved to Herdr's install in `<Herdr's state>`. `<n>` agents on Rex have not run its hooks since the move from `<old state>`, and the names made there were not carried over. In each Rex session, run Activate Namedrop again and name the panes again. An agent runs the new hooks at its next turn. Restart one that shows Hook failed or does not answer a mention. This warning clears as each of them takes a turn or ends." One agent reads "1 agent on Rex has". One line follows for each agent: `<block> on <endpoint>`. |
| Repair | "In each Rex session, run Activate Namedrop again and name the panes again. Restart an agent that shows Hook failed or does not answer a mention." |

A command of the retired state prints the short form and exits 69: `ND-INSTALL-001: Namedrop on Rex moved to Herdr's install in <Herdr's state>. In each Rex session, run Activate Namedrop again and name the panes again. Restart an agent there that shows Hook failed or does not answer a mention.`

### ND-INSTALL-002

A writer that may not change the shared agent packages reports `ND-INSTALL-002` and writes nothing. It names the install that owns them.

| Owner | Text |
|---|---|
| Herdr | `ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in <state>. Run Activate Namedrop in Herdr to update them.` |
| Another standalone install | `ND-INSTALL-002: Namedrop's shared agent packages belong to the Rex install in <state>. Use that install to update them.` |

### ND-INSTALL-003

| Field | Text |
|---|---|
| Severity | `notice` |
| Summary | "The Rex module and the shared agent packages name different installs." |
| Explanation | "An older Herdr release updated the packages without updating the Rex module." |
| Repair | "Update Namedrop in Herdr, then run Activate Namedrop there again." |

### ND-INTEGRATION-002

`ND-INTEGRATION-002` is not Rex-only. Doctor in Herdr or Rex reports it when `~/.config/namedrop/state-directory` names a state directory that an installed Codex or Claude Code `scripts/session-start` wrapper does not write. When Doctor reports `ND-INSTALL-003`, it leaves this one out.

| Field | Text |
|---|---|
| Severity | `notice` |
| Summary | "The install record names another state directory than the hooks write." |
| Explanation | "`<record>` names `<state>`, and an installed hook wrapper writes elsewhere, so Namedrop in a `<place>` would read a directory its hooks never write." `<place>` is "Rex block" in Herdr's Doctor and "Rex pane" in Rex's. |
| Repair | "Activate Namedrop again, which rewrites the wrappers and the record together." |

### ND-INTEGRATION-003

Doctor on Rex reports `ND-INTEGRATION-003` when an installed Codex or Claude Code `scripts/session-start` or `scripts/user-prompt-submit` wrapper sets no `NAMEDROP_HOOK_RUNTIME`. A release before 1.12.0 installed such wrappers.

| Field | Text |
|---|---|
| Severity | `blocked` |
| Summary | "An installed hook wrapper does not name its agent, so its sessions never bind in a Rex pane." |
| Explanation | "`<wrapper>` sets no NAMEDROP_HOOK_RUNTIME. It was installed by an older Namedrop, and an agent whose hooks do not bind cannot register or receive mentions here." More than one wrapper is listed with " and " between them. |
| Repair | "Activate Namedrop again, which rewrites the wrappers, then restart the agent." |

### ND-UPDATE-001 and ND-UPDATE-002

| Code | Severity | Summary | Repair |
|---|---|---|---|
| `ND-UPDATE-001` | `info` | "Namedrop `<version>` is available (you have `<installed>`)." | For a standalone install, "Update: download Namedrop `<version>` and run sh scripts/install.sh". A Herdr install keeps Herdr's text |
| `ND-UPDATE-002` | `info` | "Namedrop update checks are disabled by invalid settings." | "Set check_for_updates to true or false in the Namedrop settings file." |

### ND-SESSION-002

| Field | Text |
|---|---|
| Severity | `blocked` |
| Summary | "Namedrop has no session identity for pane `<block ID>`." |
| Explanation | "Namedrop has no session identity for this Rex pane. Submit the first prompt, then check registration. If identity is still missing after a turn, run Activate Namedrop again and restart the agent so it loads the Namedrop hooks." |
| Repair | "Run Activate Namedrop again, then restart this agent to load the Namedrop hooks." |

### ND-SESSION-003

`ND-SESSION-003` is not Rex-only. It reports a Codex session that runs through Codex's shared app-server, so Namedrop cannot tell which pane it runs in. [Codex 0.162 and later: the shared app-server](compatibility.md#codex-0162-and-later-the-shared-app-server) gives the fix.

### ND-SESSION-004

| Field | Text |
|---|---|
| Severity | `blocked` |
| Where | Any Rex machine, for an agent's pane that has no session |
| Summary | "Namedrop refused to bind the agent session in block `<block ID>`." |
| Explanation | "Its latest hook (`<session>`) was refused because `<reason>`." |
| Repair | "Start the agent as the block's own program, then send it one prompt so its hook binds the session." |

`<reason>` is one of these texts:

- "the hook's agent is not in the block's foreground process group, as for an agent run as a tool or a Codex shared app-server"
- "a program other than a shell runs between the block's program and the hook's agent, or the block's program is the agent's own executable, as for an agent an MCP server started"
- "the block's status record names another agent than the hook's wrapper"
- "the block's program or the hook's agent could not be read as a live process"
- "the block is already bound to its own program, and the hook ran in an agent behind it"
- "the hook ran in a Claude Code print-mode agent (CLAUDE_CODE_ENTRYPOINT=sdk-cli) behind the block's program"
- "the hook could not prove which block it runs in"
- "the binding could not be written: `<error>`"

### ND-RELAY-001

| Field | Text |
|---|---|
| Severity | `info` |
| Where | B, the machine that kept the replies |
| Summary | "Replies for another machine expired before its Namedrop acknowledged them." |
| Explanation | "This machine keeps each reply for a remote sender for 7 days, until that sender's Namedrop acknowledges it. These expired: `<count>` for @`<sender>`." `<sender>` has the form `alice@laptop`. The count also includes replies that B could not keep for A, such as replies past 10,000. |
| Repair | "Nothing needs doing. A remote sender collects replies while it has a thread open with this machine." |

### ND-RELAY-002

| Field | Text |
|---|---|
| Severity | `info` |
| Where | A, the machine that sent the messages |
| Summary | "Messages to another machine did not reach it." |
| Explanation | "Each sender got a notice in its thread with the reason. By host: `<label>`: `<n>` failed, `<n>` expired." Hosts are separated by semicolons. |
| Repair | "Nothing needs doing. Send again once the other machine can take the message." |

### ND-RELAY-003

| Field | Text |
|---|---|
| Severity | `notice` |
| Where | A, the machine that sent the messages |
| Summary | "Replies from another machine need a look." |
| Explanation | "A server can answer only for the messages this machine sent it, and Namedrop holds a limited number of replies while a thread is paused. By host: `<label>`: `<conditions>`." |
| Repair | "Resume each paused thread with a message or Continue, and check that each label in rex hosts list reaches the machine you expect." |

Each label in the explanation lists one or more of these conditions:

- "1 reply answered a message sent to another host, and was dropped", or "`<n>` replies answered a message sent to another host, and were dropped".
- "Namedrop holds as many of its replies as it can for threads the Automatic Reply Limit paused, so it keeps the rest until a person's message or Continue resumes them"
- "it let go of 1 reply for this machine before this machine acknowledged it", or "it let go of `<n>` replies for this machine before this machine acknowledged them"
- "it keeps no more replies for this machine, so Namedrop sends it nothing new until this machine collects them"
- "its server answers as the one `<other>` reaches, which rex hosts list gives other addresses, so Namedrop relays nothing by `<label>` until the two list the same addresses or one is removed"

### ND-RELAY-004

| Field | Text |
|---|---|
| Severity | `info` |
| Summary | "Namedrop set aside a relay database from an unreleased build." |
| Explanation | "relay.sqlite3 had tables that no release of Namedrop makes, so Namedrop moved it aside and started a new one. Messages to or from another machine that were in flight then stayed in it. Set aside in the state directory: `<files>`." |
| Repair | "Nothing needs doing. Delete the file once you no longer need it." |

### ND-RELAY-005

| Field | Text |
|---|---|
| Severity | `notice` |
| Summary | "Relay conditions could not be read." |
| Explanation | The error. |
| Repair | "Check the relay database and filesystem permissions, then run Doctor again." |

### ND-THREAD-001

`ND-THREAD-001` is not Rex-only. On Rex it is how you continue a thread that the Automatic Reply Limit paused, because Rex has no Namedrop screen for it. Its repair is a `namedrop thread continue` command with the thread's ID.

## Limits of Rex support

- **Experimental.** Rex is a preview, and Namedrop's Rex support can change between releases. Namedrop 1.12.0 is the first release that includes it.
- **Channels names Herdr.** **Open Namedrop Channels** opens a page that tells a Rex user to open Herdr: "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration.", "Open this workspace in Herdr and choose Namedrop Doctor to review this item.", "Answer it in its pane in Herdr.", "Rename it in Herdr.", and "This workspace has no label in Herdr." Rex has no counterpart for these Herdr steps. The palette actions and `namedrop` commands do that work on Rex.
- **Channels can't create channels or add agents in Rex.** **New channel** and **Add agent** open a Herdr workspace or tab, so in a Rex session each form says "Channels can't create a channel in Rex yet." or "Channels can't add an agent in Rex yet." and opens nothing. Start the Rex session or pane yourself, then activate and name it there.
- **Channels' tailnet link does not come back by itself.** With Tailscale access on, Herdr starts the Channels server again when Herdr starts. Nothing in Rex does, so after a reboot run `namedrop channels --no-open`. Doctor's `ND-TAILSCALE-001` says so.
- **Rex can miss a new session's first events.** Rex sometimes sends no startup events, such as `block_created`, for the first pane of a new Rex session. Namedrop loses nothing: activation reads the Rex session's panes itself, and that pane's later events still arrive.
- **The Rex app's palette was tested on one build.** On Rex Server preview.23, the Rex app's palette lists the eleven actions. Activate Namedrop, Name this pane and About Namedrop were run from it, and the Name this pane, Manage, Agent status, tour and Doctor keys opened their screens. About Namedrop shows nothing when you run it from the palette. Other Rex builds were tested only with `rex do --origin palette`, which proves what the actions do, not what the app shows.
- **Windows is out of scope.** `install.sh` runs only on macOS on Apple silicon and on Linux, and a Linux build needs glibc. B's relay accepts only a Tailscale principal or a local peer with B's user ID, so it refuses other callers, such as a Windows peer.
- **Remote mentions only through `namedrop send`.** A remote address typed at the start of an agent's prompt is not a mention.
- **The relay reads hosts from Rex's default socket.** The relay lists A's hosts with `rex hosts list`, which ignores `-S` and `REX_SERVER`. It reads your Rex home and the Rex server on this machine's default socket. With A's server on another socket, and no other home chosen, each remote send fails with `could not read your Rex hosts: <error>`.
- **A sender's host is the sender's own claim.** Unless Rex identifies the caller as a Tailscale principal, the host in a relayed sender's name, such as `@alice@laptop`, is what the sending machine says. B does not check it, and tells its agent so.
- **No delivery confirmation across machines.** A learns that B accepted a message. A does not learn when B's agent received it.
- **Two labels for one server.** A compares the endpoints that `rex hosts list --json` gives each label, as Rex dials them. If two labels reach one server through endpoints that differ, A relays nothing through the second label, and `ND-RELAY-003` names both. Relaying resumes when the two labels list the same endpoints, or when one of them is removed.
- **Your Rex home decides which labels exist.** The hosts that `rex hosts list` prints come from your Rex home and from this machine's server. Their entries decide where your remote mentions go.
- **B started without restore retires its names.** If B's Rex server starts with `--restore=false`, B's bridge finds B's Rex sessions gone and retires their names. After B's next normal start, its panes need to be named again.
- **Names made on Rex do not move to Herdr.** After a move from Rex to Herdr, the old names stay in the old state directory.
- **Each relay call opens its own ssh connection.** Over `ssh://`, each `rex` call that the relay makes starts a new ssh tunnel. In the remote-host lab, a call took about 125 ms, and A started 3 tunnels at each 30-second collect. On a slow link, each tunnel is a full ssh handshake.

[What has been tested on Rex](compatibility.md#what-has-been-tested-on-rex) records what has been tested, and what has not.
