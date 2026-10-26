# Native recipes (experimental branch)

`control-namedrop.py` drives the cross-workspace and dashboard features in a disposable
native Herdr with no model: it fakes agent sessions, so it proves routing, envelopes and
database state, not how a real agent reads them. Pair it with a real-agent drive on a
kept testbed profile.

From the repository root:

```sh
cargo build --locked --bin namedrop
.claude/skills/verify/native/control-namedrop.py --feature cross-workspace --binary target/debug/namedrop
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
- [dashboard.md](dashboard.md): the live dashboard server.
