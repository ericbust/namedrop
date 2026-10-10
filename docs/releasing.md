# Build a release

Start from a reviewed commit. Build each archive on the machine it's for: `aarch64-apple-darwin` on an Apple silicon Mac, and `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu` on GNU/Linux of that architecture. The packager runs the executable to check its version, so it refuses one built for another platform. The Linux aarch64 archive stays a prerelease until someone tests it by hand.

Set the same version in `Cargo.toml` and `herdr-plugin.toml`. The packager refuses a mismatch between those two, or between either and the built executable's `--version`. It doesn't read `Cargo.lock`, so refresh that too. Nothing checks versions written in prose, so search `README.md`, `CONTRIBUTING.md` and `docs/*.md` for the old archive name and version by hand.

Check that the crate includes the source, lockfile and documentation:

```sh
CARGO_TARGET_DIR=target/crate-checks cargo package --locked --allow-dirty
```

Use `--allow-dirty` only while you review locally. Release from a clean checkout. The separate target directory keeps packaged-source fingerprints out of your normal builds.

Build the archive for this machine (`--target` names it explicitly):

```sh
sh scripts/package-release.sh
cd target/dist
shasum -a 256 -c namedrop-<version>-aarch64-apple-darwin.tar.gz.sha256   # Linux: sha256sum -c namedrop-<version>-<target>.tar.gz.sha256
```

The builder uses `target/release-build`, checks the architecture and version, removes the build step from the distributed manifest, and packs the executable, the manifest, `runtime.json`, the setup, update, and launcher scripts, the documentation, the license, and checksums. It never overwrites existing output. For another local build, pick a fresh directory with `--output`. To package an executable you've already built for the target, pass `--binary`.

It refuses a binary that is missing, not executable, the wrong format (it must be an arm64 Mach-O, or an ELF for the Linux architecture using glibc's dynamic loader), or whose `--version` disagrees with `Cargo.toml` (exit 65). It also refuses to write over an existing archive or checksum (exit 73).

## Verify the archive

Run the release tests:

```sh
CARGO_TARGET_DIR=target/release-checks cargo test --locked --test release
```

They run the builder on a **stand-in** executable with a stubbed `file`, then check the archive it made:

- Every `[[startup]]`, `[[actions]]`, `[[events]]` and `[[panes]]` entrypoint survives packaging with the same arguments, and routes through `scripts/run.sh` to `bin/namedrop` or the stable runtime. This includes the `naming` popup and its `ui launch` / `ui run` commands.
- Commands run from the extracted directory, and activation has its own runtime lifecycle check.
- The executable and setup helper are still executable after extraction, and every packaged file has a checksum.
- Installing works from a directory with spaces in its path, with no `PATH` entry and without Cargo.
- Tampering, a foreign architecture, a version mismatch and an overwrite are refused with the exit codes above.

Because the executable is a stand-in, these tests prove the packaging and paths, not that the real binary handles each command. Check a real build with the steps below, and note the results for each entrypoint from an installed archive.

Then check the archive the way a user would, from the download directory:

```sh
shasum -a 256 -c namedrop-<version>-aarch64-apple-darwin.tar.gz.sha256
tar -xzf namedrop-<version>-aarch64-apple-darwin.tar.gz
cd namedrop-<version>-aarch64-apple-darwin
shasum -a 256 -c SHA256SUMS
file -b bin/namedrop
./bin/namedrop --version
```

On Linux, use `sha256sum -c` for both checksum files. The outer checksum covers the archive. `SHA256SUMS` covers every file inside it. The builder doesn't sign or notarize anything, and a locally built archive doesn't go through macOS quarantine, so test a downloaded copy before saying downloads work.

## Publish

1. Complete the source and archive checks in [CONTRIBUTING.md](../CONTRIBUTING.md). Then install the archive in a fresh Herdr profile, start new Codex and Claude Code sessions, and send a mention each way. Update the tested versions in [compatibility](compatibility.md).
2. Write down the source commit, the executable's SHA-256 and the archive's SHA-256 together. Tag the reviewed commit whose packaged files match the archive. If you change any packaged file, rebuild and check the checksums again. Even a change to the documentation alone gives the archive a new checksum.
3. Write release notes covering the changes, compatibility, [upgrade and rollback](operations.md#upgrade), and anything that failed, was blocked or wasn't tested. To show the tour to existing users again, ship a new [tour presentation](operations.md#tour-presentations) with a new id and point `CURRENT_ID` at it; never reuse an id. Breaking changes to the runtime integration need fresh agent sessions. Compatible updates let existing sessions keep what they loaded, and new sessions pick up the new instructions.
4. Attach the archive and its `.sha256` file to the GitHub release. If testing isn't finished, say so and publish it as a draft or prerelease.

CI runs formatting, Clippy, the tests, crate packaging and archive building on GitHub's `macos-15` (Apple silicon), `ubuntu-24.04` (x86_64) and `ubuntu-24.04-arm` (aarch64) runners. The manual Release artifacts workflow builds one archive per target but doesn't publish a release. CI has no Herdr, no agents and no credentials, so the native Herdr tests and the test with real agents have to be run by hand.

## Live-update compatibility

`runtime.json` is a promise about compatibility. Keep `state_schema` and `collaboration_protocol` matching the store and the protocol. Increase `live_upgrade_epoch` whenever old in-flight commands, cached instructions, hook behavior or permission rules can't safely run alongside the new runtime. Only list a schema in `live_migrate_from` if its migration runs behind the command gate (`command_gate: 1`). Keep the v1 stable launcher, and route host startup, actions and events through `scripts/run.sh`; changing that launcher protocol needs a migration.

Run `cargo test --locked --test seamless_updates` and `cargo test --locked --test update`, which runs `tests/update_lifecycle.py`. The first one runs `tests/live_schema.py`, which tests upgrades from releases that use the command gate on every platform, from older releases without it on macOS only, and on Linux checks that a release without the gate is refused and left unchanged.

On macOS or Linux with native Herdr and permission to inspect processes, run `cargo build --locked`, then `python3 tests/seamless_native.py` once with `NAMEDROP_TEST_SHELL=bash` and once with `zsh`. It uses its own HOME, state, socket, local Git origin and a simulated agent, and makes no model calls. It must keep the session identity, send a real delivery and reply across the checkout swap, and refuse a branch that moved before the swap. Checking that real agents load the new hooks is a separate manual step.
