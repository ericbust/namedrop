# Namedrop on Rex reference

This page describes Namedrop's experimental Rex support on the `experimental` branch. To set it up, see [Run Namedrop on Rex](rex.md). For the Rex versions that Namedrop accepts, see [Compatibility](compatibility.md#rex-experimental).

## Terms

| Term | Meaning |
|---|---|
| Space | One Rex session. Names are per space |
| Block | A Rex pane. Its ID has the form `block:<uuid>` |
| Bridge | `namedrop rex bridge`, one background process for each Rex server. It reads the server's events, retires the names of closed blocks, delivers queued mentions, and answers the relay's calls. Any Namedrop command or hook in a Rex block starts it when it is not running |
| A | The machine that sends a mention to another machine |
| B | The machine whose agent receives that mention |
| Relay worker | `namedrop relay worker`, one background process on A for each host label. It sends A's queued messages to B and collects B's replies |

## Remote addresses

A remote address is `@<name>@<label>`.

- `<name>` is a name in a Rex session on B, at most 64 bytes.
- `<label>` is a label that `rex hosts list` prints on A, matched letter for letter. It is 1 to 64 ASCII letters, digits, `-`, and `_`.
- Only the leading recipients of `namedrop send` can be remote. In an agent's prompt, and in a message body, `@name@label` is not a remote address.
- A send to a remote address carries text only. It refuses `context`, `request_context`, tracked work, and `in_reply_to`, with `a message to an agent on another host is a plain send: it carries no context and no tracked work`.
- A send to a label that `rex hosts list` does not print fails at once with `<label> is not in your Rex hosts (rex hosts list): <labels>`, and Namedrop queues nothing.
- A relayed message shows its sender as `@<name>@<host>`. `<host>` is what A's Namedrop says its machine is called, which is A's host name up to its first dot.

## Relay timings and sizes

| What | Value |
|---|---|
| How often A collects replies while a thread with B is open | Every 30 seconds, and with each message A sends |
| How long a thread stays open | 24 hours after the last message B accepted or the last reply A collected |
| How long A tries to deliver a message | 24 hours. Then it fails with `<label> was unreachable for 24 hours` |
| Wait between tries after a fault | 1, 2, 5, and 10 seconds, then 30 seconds for each later try |
| How long one `rex` call may take | 15 seconds. A message whose call timed out is sent again with the same ID, and B accepts it once |
| Largest message A sends | A request of 60 KiB. A larger send fails at once with `the message to @<name>@<label> is too large to relay` |
| Largest reply text | 60 KiB. A longer reply is cut and ends with `[Namedrop cut this message to relay it.]` |
| Largest reason A keeps from B | 1 KiB |
| How long B keeps a reply for A | 7 days |
| Most replies B keeps for one A host | 10,000. B keeps no reply past that and counts it in its own `ND-RELAY-001` |
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
| `the relay does not accept a <kind> caller here` | B's Rex server admitted the caller as something other than a Tailscale principal or a local peer with B's user ID |
| `<label> now answers as another Rex server` | The label reaches a different Rex server than when A first sent the message |
| `<label> runs a Namedrop without this relay` | B's Namedrop has no relay, or a different one |
| `<label> is no longer in your Rex hosts` | Someone removed the label after the send |
| `<label> keeps no more replies for this machine until this machine collects them` | B keeps 10,000 replies for A. Each message that A had not sent yet fails |
| `<label> was unreachable for 24 hours` | B did not answer for 24 hours |

While B's bridge is stopped or B cannot be reached, A keeps the message and tries again. A refusal is stored with the message, so a later change on B, such as a new `relay/allow-terminals`, does not change the answer to that message.

## Files in the state directory

The state directory is `$NAMEDROP_STATE_DIR`, or else the directory recorded in `~/.config/namedrop/state-directory`, or else `$XDG_STATE_HOME/namedrop` or `~/.local/state/namedrop`.

| File | Effect |
|---|---|
| `relay/allow-terminals` | On B, lets a relayed mention reach a terminal, which runs it as a shell command. Off when the file is missing |
| `relay.sqlite3` | The relay's own database: messages to and from other machines, held replies, and what Doctor reports |

## Doctor on Rex

These diagnostics appear only in a Rex space. `ND-RELAY-001` to `ND-RELAY-003` read the relay's database. Each one leaves the repair to you.

### ND-RELAY-001

| Field | Text |
|---|---|
| Severity | `info` |
| Where | B, the machine that kept the replies |
| Summary | "Replies for another machine expired before its Namedrop collected them." |
| Explanation | "This machine keeps each reply for a remote sender for 7 days, until that sender's Namedrop collects it. These expired: `<count>` for @`<sender>`." `<sender>` has the form `alice@laptop`. The count also includes replies that B could not keep for A, such as replies past 10,000. |
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

- "1 reply answered a message sent to another host, and was dropped", or "`<n>` replies answered a message sent to another host, and were dropped". A counts these until 7 days pass with none.
- "Namedrop holds as many of its replies as it can for threads the Automatic Reply Limit paused, so it keeps the rest until a person's message or Continue resumes them"
- "it keeps no more replies for this machine, so Namedrop sends it nothing new until this machine collects them"

### ND-SESSION-004

| Field | Text |
|---|---|
| Severity | `blocked` |
| Where | Any Rex machine, for an agent's block that has no session |
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

### ND-THREAD-001

`ND-THREAD-001` is not Rex-only. On Rex it is how you continue a thread that the Automatic Reply Limit paused, because Rex has no Namedrop screen for it yet. Its repair is a `namedrop thread continue` command with the thread's ID.

## Limits of the experimental build

These limits hold at the time of writing. The first Rex release must resolve them or document them.

- **No installer and no Rex screens.** Rex has no Namedrop installer, palette actions, key bindings, or popups yet. They are planned. `activate` on Rex still writes Herdr key bindings to `~/.config/herdr/config.toml`.
- **Some text names Herdr.** On Rex, some Doctor texts and the command help still say Herdr, such as `ND-SESSION-002`, "Herdr has no session identity for pane `<block ID>`.", `ND-SETUP-003`, "Start Codex or Claude Code in a Herdr pane, then check again.", and `activate --help`, "Activate Namedrop for this Herdr space after confirmation". The update notice that agents and Doctor show gives a Herdr command, `herdr plugin action invoke update --plugin namedrop`. The agent skill's description says "in Herdr", and the skill says that registration uses Herdr flags, where Rex uses `--endpoint`, `--space`, and `--pane`. A planned change replaces these texts.
- **Remote mentions only through `namedrop send`.** A remote address typed at the start of an agent's prompt is not a mention.
- **A sender's host is the sender's own claim.** The host in a relayed sender's name, such as `@alice@laptop`, is what the sending machine says. B does not check it.
- **A stopped relay worker waits for a restart.** If the relay worker on A stops while a thread is open, it starts again only when the bridge connects or the next message to that host is sent. B's replies wait on B until then.
- **The relay's database format can still change.** A `relay.sqlite3` made by an earlier experimental build may fail to open. Before the first release, its versioning still has to be settled.
- **Run the same build on both machines.** Which pairs of Namedrop builds work together is not settled. A B without this relay fails each message with `<label> runs a Namedrop without this relay`. A B from an older relay build can make A read the same page of replies again on each round past a full thread.
- **A does not learn what B threw away.** B keeps at most 10,000 replies for one A host. B's Doctor counts the rest and the replies that expired, but A learns only that B keeps no more.
- **Two labels for one server.** A treats two labels as one server only when `rex hosts list --json` gives them exactly the same endpoints. If two labels spell one server's address differently, a reply to a message sent through the other label is dropped, and `ND-RELAY-003` counts it. Give each server one label, or the same endpoints.
- **Your Rex home decides which labels exist.** The hosts that `rex hosts list` prints come from your Rex home and from this machine's server. Use a Rex home that you trust, because its entries decide where your remote mentions go.
- **No delivery confirmation across machines.** A learns that B accepted a message. A does not learn when B's agent received it.
- **Not tested.** TLS to a real remote Rex server, B on Linux, and two real machines. The relay was tested with two Rex servers on one Mac. Nobody has checked yet whether Rex answers `not_found` for the blocks of a host that is away.
