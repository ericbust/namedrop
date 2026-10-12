# Stale runtime

A bare `herdr plugin install`, or any reinstall that is not Namedrop's own Update action, rebuilds `<plugin_root>/bin/namedrop` and never activates it. Namedrop keeps running the release that `<state>/runtime/current` points at, because every entrypoint goes through the stable launcher and `src/main.rs` re-execs it. Doctor must report the mismatch and offer a repair that switches the active release.

## Sub-features

- `stale-runtime-flag` reports `ND-SETUP-004` when the launcher exists and the digest of the registered root's `bin/namedrop` differs from the active release's digest.
- `stale-runtime-repair` carries the repair command `sh <plugin_root>/scripts/setup.sh activate --human`. Running it in the workspace switches `runtime/current` to the reinstalled executable.
- `stale-runtime-clear` shows no `ND-SETUP-004` once the active release matches the registered root again.

## How to get to it (user POV)

- Install or update Namedrop with `herdr plugin install ericbust/namedrop --ref main` from a shell, without opening Update Namedrop or Activate Namedrop afterwards.
- Open Namedrop Doctor, or run `namedrop doctor`.
- Before the fix, Doctor reported `needs_attention` with only the issues the space already had. On the model-free fixture that is `['ND-SETUP-003']`. Nothing mentioned the newer build in `bin/`, and `runtime/current` still pointed at the previous release.
- The wanted behavior adds `ND-SETUP-004` with the registered root's built executable as `diagnostic.path` and the repair command above. After the repair, Doctor shows the space's other issues only.

## Driving it with control-namedrop

Preconditions: the same as [stuck wake](stuck-wake.md). The helper links the candidate as a disposable plugin and activates it through `scripts/setup.sh activate --json`, so `<state>/runtime/current` holds the candidate.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature stale-runtime --binary <candidate>`.
- The helper stages a reinstalled executable at `<root>/bin/namedrop` the way `scripts/build.sh` does, by renaming a complete copy into place. The copy is the candidate plus one trailing newline, which changes its digest and leaves the ELF program headers intact. The helper requires the copy to answer `--version` and requires `runtime/current` to still name the candidate's digest.
- Doctor runs from the operator pane as `doctor_stale`. The recipe requires an issue with code `ND-SETUP-004`. At commit b1b9ec3 this is where the run stops, with `stale runtime: doctor flags the stale runtime as ND-SETUP-004 while bin/ and current/ differ; observed ['ND-SETUP-003']`.
- The recipe requires that issue's `repair.command` to equal `["sh", "<root>/scripts/setup.sh", "activate", "--human"]`, then runs it with the fixture's workspace, pane, socket and plugin-state context. It requires exit 0 and requires `runtime/current` to name the reinstalled digest afterwards.
- Doctor runs again as `doctor_repaired` and the recipe requires its issue codes to equal `['ND-SETUP-003']`.
- `stale-runtime.json` records the candidate, reinstalled and active digests, both Doctor reads and the repair's output, on every stop. `summary.json` carries the failing requirement as `outcome`.

## Limits

- The reinstalled executable is the candidate with one extra byte, not a different revision, so the drive proves digest comparison and the switch, not a behavior change between releases.
- The fixture has no model sessions, so `ND-SETUP-003` stays present on every Doctor read.
