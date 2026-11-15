# Herdr differential
Run both binaries from archive builds with their `runtime.json` two levels above the binary, as in `target/debug/namedrop`.
```sh
env -u REX_BLOCK -u REX_SESSION -u REX_SERVER -u REX_CLIENT python3 -I tests/differential/run.py BASE_BINARY HEAD_BINARY OUT
```
The runner executes each binary three times in separate profiles. It compares every base report with every head report, and checks consistency within each build. A difference exits 1. Permission failures are inconclusive, never a pass.
`flow.py` comes from the newest retained `scratchpad/s4impl/relay-harden/flow/flow3.py`. That task's `tools/` has no flow script. `herdr` logs calls and location environments, and `launcher` logs self-spawns. The flow includes activation, naming, registration, sends, hooks, deferred delivery, reminders, pause/resume, Doctor, and Channels. It snapshots every file under HOME and state, with each file's mode. Binary and SQLite contents are represented by path only. Text uses the same normalization as command output. An id after a known prefix, such as `delivery-`, is normalized only when it contains a digit, so names like `delivery_inbox` stay as they are.
`cmp.py` drops the scrubbed agent and Rex variables while preserving both timeouts. It compares each step's stdout and stderr in order, and the `(set)` sections as sets. It permits only the documented transient badge with `namedrop-awaiting=4` and `namedrop-attention=2`. All other differences fail.
The script host handles the differential only. It does not replace either real-agent testbed lane. The run needs `/bin/ps` and `sandbox-exec`; a sandbox that blocks either cannot produce acceptance evidence.

U3 also runs deactivate, activate again, and name in a second inactive space. Pass `--allow-package-lock` to the runner only for U3. It permits the added empty HOME `.config/namedrop/packages.lock` file, mode 0600, and preserves every other comparison.

U5 also runs `ui launch name` and `ui launch doctor` from a pane, after activation. Nothing is pinned.
