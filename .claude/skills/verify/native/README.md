# Native recipes (experimental branch)

`control-namedrop.py` drives the cross-workspace, tracked-handoff, remote-terminal and dashboard features in a disposable
native Herdr with no model: it fakes agent sessions, so it proves routing, envelopes and
database state, not how a real agent reads them. Pair it with a real-agent drive on a
kept testbed profile. For cross-workspace mentions that drive is two scripts,
`.claude/skills/verify/scripts/second-workspace.py` and then
`.claude/skills/verify/scripts/cross-workspace-probe.py`; **What the harness does not
reach** in [../SKILL.md](../SKILL.md) says what they check.

From the repository root:

```sh
cargo build --locked --bin namedrop
.claude/skills/verify/native/control-namedrop.py --feature cross-workspace --binary target/debug/namedrop
.claude/skills/verify/native/control-namedrop.py --feature cross-workspace-tracked --binary target/debug/namedrop
.claude/skills/verify/native/control-namedrop.py --feature remote-terminal --binary target/debug/namedrop
.claude/skills/verify/native/control-namedrop.py --feature dashboard
```

Requires Python 3.9+, Git, Bash and native Herdr on PATH. The helper starts its own
`herdr server` with an isolated home, configuration, state and socket under
`/tmp/nd-verify-*`, links the candidate as a disposable plugin, and tears everything down
in `finally`, failing the run if a leftover process had to be stopped. Evidence and the
JSON summary land under `.scratch/verify/`. It never touches the real `~/.herdr`,
`~/.claude` or `~/.codex`.

- [cross-workspace.md](cross-workspace.md): `#label @name`, `#all`, `#repo`, the pickers
  and replies across workspaces.
- [cross-workspace-tracked.md](cross-workspace-tracked.md): `handoff begin` in one
  workspace, a tracked send to `#label @name`, the worker's bound reply, `handoff results`
  and `resolve` in the lead's workspace, and coordination rows that stay there.
- [remote-terminal.md](remote-terminal.md): a named terminal that refuses another
  workspace's command until a person there runs `address allow-remote`, the allowed run
  with its result home in the sender's `handoff results`, and `address deny-remote`.
- [dashboard.md](dashboard.md): the live dashboard server.
