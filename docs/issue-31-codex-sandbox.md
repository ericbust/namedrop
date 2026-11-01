# Codex reply sandbox investigation

Rounds 1 and 2 of [ericbust/namedrop#31](https://github.com/ericbust/namedrop/issues/31) found that sandbox settings alone do not solve this on Linux with Codex 0.164.0. Adding only Namedrop's plugin state directory removes the database write failure, but a direct reply still fails when it connects to Herdr's Unix socket. Round 3 adds a state-only command handoff to an outside Namedrop worker.

The initial writable-root implementation was removed after its direct-delivery experiment failed. It is now restored as part of the command handoff, with activation, read-only doctor inspection, and ownership-aware cleanup on the last deactivation. A preexisting user grant is preserved.

## Round 3 delivery handoff

Before a mutating command, Namedrop probes its Herdr transport. If the sandbox denies the connection, a registered agent can publish a bound request under the plugin state directory. The bridge accepts send, reply, ack, complete, and a current-workspace roster read. It stores the full validated input, exact caller identity and original request ID; it accepts no arbitrary shell or Herdr commands. The sandbox command waits briefly for the durable result. If no result arrives, it returns an explicit pending receipt whose commit status is unknown.

A persistent worker started from Herdr-owned lifecycle entrypoints polls pending markers and executes each request outside the agent sandbox. The existing routing, current native session, registration and delivery-attempt checks still run. Each bounded execution holds the normal runtime update guard; the idle worker does not block upgrades. Herdr lifecycle events can restart the worker, and queued requests survive its restart. The implementation changes no network policy.

Stable request IDs and durable receipts prevent a retry from creating another logical message. Recovery also rejects changed coordination fields and can recover an existing commit after its original session ends. Recipient dispatch retains Namedrop's existing ambiguous-attempt holds. Herdr has no prompt idempotency token, so this does not claim universal exactly-once physical prompting across the socket-call crash window.

Both final live checklists passed all nine steps on the same Rust code. Actual Codex calls showed no escalation request, read-only filesystem error or socket permission error. The sessions retained restricted networking and added only the plugin state directory to the default writable paths.

| Live case | Queue to recipient receipt | Result |
| --- | --- | --- |
| Default approval reviewer | 1.944 seconds | One reply, one recipient delivery attempt |
| Automatic approval reviewer | 0.987 seconds | One reply, one recipient delivery attempt, no approval requested |
| Worker stopped before execution, then restarted | 0.962 seconds | One reply; deleting its result receipt and replaying still created no duplicate |

These are individual functional acceptance trials, not a latency percentile or availability guarantee. A command's result receipt can arrive after its recipient receipt while the outside executor finishes mailbox work. The two-second producer timeout reports that interval as pending with an unknown commit status, and an exact retry recovers the original result.

## Settings-only experiments

The testbed used Herdr 0.9.1, Codex 0.164.0 with `gpt-6-sol`, and Claude Code 2.1.310. Codex ran with `--no-daemon`. Every run used a separate testbed profile and a socket under `/tmp/nd-tb-`. The real Codex and Claude configuration directories were not edited. No tool permission prompt was answered manually.

| Configuration | First Codex reply | Next action | Acceptance gate |
| --- | --- | --- | --- |
| Main, default reviewer | Exit 69, `Read-only file system (os error 30)` | Codex requested elevated execution and waited for approval | Failed |
| Exact state writable root, default reviewer | Exit 65, `PermissionDenied`, `Operation not permitted` | Codex requested elevated execution and waited for approval | Failed |
| Exact state writable root, `auto_review` | Exit 65, the same socket error | Codex requested elevated execution, and automatic review allowed the retry | Failed, although the ordinary checklist passed 9/9 |

The patched sessions' own `thread_settings_applied` records include the exact plugin state directory as writable. The default run records `approvals_reviewer = "user"`; the automatic run records `"auto_review"`. This verifies the active session policy rather than inferring it from the config file.

A separate direct probe inside `codex sandbox` created an `AF_UNIX` socket successfully, then failed at `connect(testbed_socket)` with `PermissionError: [Errno 1] Operation not permitted`. Running `herdr pane list` under the same policy returned the same error. The probe used a named permissions profile extending `:workspace`, with only the exact Namedrop state directory added to its filesystem policy. Adding an exact `network.unix_sockets` entry alone also left the connection denied. No broader network setting was applied to the integration.

## Round 2 follow-up with the network enabled

The first socket probe omitted `network.enabled = true`. A follow-up tested that setting explicitly, with the exact state-directory write grant, only the Herdr socket in `network.unix_sockets`, and an empty `network.domains` table. Each configuration ran with `network.mode` omitted, `"limited"`, and `"full"`.

| Proxy feature | State write | Herdr socket | Outbound `curl https://example.com` |
| --- | --- | --- | --- |
| Disabled, all three modes | Succeeds | Connects | Succeeds, exit 0 |
| Enabled, all three modes | Succeeds | Socket creation fails with `EPERM` | Blocked with a proxy 403, exit 7 |
| Host control | Succeeds | Connects | Succeeds, exit 0 |

`network.enabled = true` alone allows the requested local connection, but it also allows outbound internet. Empty domain rules do not enforce a restriction while the proxy feature is off. Enabling `[features].network_proxy = true` enforces those rules. With the proxy enabled, a direct request using `curl --noproxy '*'` also fails, with exit 6.

One fresh live Codex run used the default approval reviewer, the proxy-enabled policy, and `mode = "full"`. The actual reply exited 65 with `Operation not permitted`, then requested elevated execution and displayed an approval prompt. Its session records identify the reviewer as `"user"`. No tool permission was approved. The configuration therefore passes the outbound restriction check and fails both the local IPC and approval-free reply checks. No runtime change was restored, and the conditional automatic-review rerun was unnecessary.

The installed-version [Linux seccomp implementation](https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/linux-sandbox/src/seccomp.rs#L180-L273) explains the difference. Restricted mode denies `connect`. Proxy-routed mode denies standalone `AF_UNIX` socket creation unless its broad Unix-socket override is enabled. The exact `unix_sockets` path allowance does not change that syscall filter. The [proxy documentation](https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/network-proxy/README.md#L234-L239) also states that its `x-unix-socket` forwarding route is macOS only.

The follow-up never set legacy `network_access = true` or enabled `dangerously_allow_all_unix_sockets`. No tested policy allowed the exact local connection while blocking outbound internet.

## What Codex supports

The installed CLI advertises per-session `--add-dir`. Its version-tagged [config schema](https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/core/config.schema.json) accepts `sandbox_workspace_write.writable_roots` as an array of absolute paths. The [official configuration reference](https://developers.openai.com/codex/config-reference/) documents additional writable roots for workspace-write mode and separate Unix socket network permissions.

An isolated live session confirmed that the legacy writable-root setting takes effect with the default implicit workspace policy. Explicitly selecting `:workspace` in the standalone sandbox command ignores that legacy customization, as the installed-version [configuration implementation](https://github.com/openai/codex/blob/rust-v0.164.0/codex-rs/core/src/config/mod.rs) explains. The direct socket probe therefore used an explicit profile containing the filesystem grant.

The [official rules documentation](https://developers.openai.com/codex/rules/) says an allow rule can run a matching command outside the sandbox without prompting. `codex execpolicy check` matched Namedrop's installed exact reply prefix with `decision = "allow"`. Nevertheless, the live code-mode `tools.exec_command` calls ran sandboxed first and then requested approval. The dry-run match does not explain why the live execution did not bypass the sandbox. That discrepancy needs upstream investigation.

## Upstream report draft

Suggested title: **Linux sandbox cannot allow one Unix socket while blocking outbound internet in Codex 0.164.0**.

To reproduce with Namedrop main at `3864c16`:

1. Create a fresh isolated Namedrop testbed profile with both Claude Code and Codex. Keep Codex's default approval reviewer and run Codex with `--no-daemon`.
2. In that profile's `CODEX_HOME/config.toml`, add its exact `state/herdr/plugins/namedrop` directory to `sandbox_workspace_write.writable_roots`. Do not change network access or approval policy.
3. Start a fresh Codex session and complete the testbed's hook trust setup.
4. Ask Claude to send Codex a Namedrop delivery requesting the word `pong`.
5. Observe the first reply fail with exit 65 and `Operation not permitted`, followed by an approval prompt for elevated execution.
6. Check the reply prefix against the profile's `rules/namedrop.rules` with `codex execpolicy check`. It returns `allow`.
7. Repeat in a fresh profile with `approvals_reviewer = "auto_review"`. The first command still fails and escalates; an approved retry can make the ordinary checklist pass.
8. In a throwaway permissions profile extending `:workspace`, enable its network, add only the exact Herdr socket to `network.unix_sockets`, and leave `network.domains` empty. Without the proxy feature, both the socket and outbound HTTPS work. With the proxy feature enabled, outbound HTTPS is blocked and standalone Unix socket creation fails. Selecting either documented network mode leaves these results unchanged.

Expected behavior: an integration should have a supported way to send an authorized local reply without a failed sandbox attempt or an approval decision on every delivery.

Questions for upstream: is there a supported Linux policy that allows one standalone Unix socket while denying outbound internet, and why does the live execution differ from the exact-prefix policy check? A writable directory alone does not satisfy the observed IPC requirement, and the enabled-network variants tested here do not satisfy both requirements together.

## Evidence and remaining work

The maintainer's local evidence is under `~/.local/state/pstack/orchestrate/namedrop-ui/reports/ISSUE-31/`. `baseline/`, `default/`, and `auto-review/` contain pane captures and Codex rollouts. `run-live.py` reruns the isolated checklist with an explicit reviewer choice. `socket-probe.py` reproduces the socket denial without an agent or approval prompt. `ISSUE-31.json` in the parent reports directory records the final status and checks.

The follow-up evidence is under `round2/`. `network-probe.json` and `proxy-network-probe.json` contain the enabled-network mode matrix and host controls. `network-probe.py` reruns it against an isolated Herdr testbed server. `run-live-network.py` configures the proxy-enabled policy before launching the default-reviewer live checklist.

Round 3 evidence is under `round3/`. `design.md` records the command and process boundaries. `run-live.py` retains real Codex calls, immutable queue requests, durable receipts and the recipient database. `assess-live.py` checks the exact filesystem grant, restricted network, absence of escalations and socket errors, and one recipient delivery per new logical message. `restart-probe.py` checks a real queued reply across worker restart and replays a committed command after losing its receipt.

The original settings-only implementation remains in branch history at `0936e1e` and `e14ae68`. Its writable-root grant is useful together with the outside command worker. The upstream draft above describes the remaining Linux sandbox capability limit; it does not require Namedrop replies to use a broader network policy.
