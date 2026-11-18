#!/bin/sh
set -eu
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
mkdir "$test_root/scripts"
cp "$(dirname "$0")/../scripts/release.sh" "$test_root/scripts/release.sh"
git -C "$test_root" init -q

set_version() {
  printf '[package]\nname = "namedrop"\nversion = "%s"\n' "$1" > "$test_root/Cargo.toml"
  printf '[[package]]\nname = "namedrop"\nversion = "%s"\n' "$1" > "$test_root/Cargo.lock"
  printf 'id = "namedrop"\nversion = "%s"\n' "$1" > "$test_root/herdr-plugin.toml"
  printf '# Changelog\n\n## [%s] - 2026-11-02\n\n- Test release.\n' "$1" > "$test_root/CHANGELOG.md"
}

check_passes() {
  set_version "$1"
  sh "$test_root/scripts/release.sh" check HEAD > /dev/null
}

check_fails() {
  set_version "$1"
  if sh "$test_root/scripts/release.sh" check HEAD > /dev/null 2>&1; then
    printf 'release check accepted %s above base %s\n' "$1" "$2" >&2
    exit 1
  fi
}

set_version 1.5.0-exp.2
git -C "$test_root" add Cargo.toml Cargo.lock herdr-plugin.toml CHANGELOG.md
git -C "$test_root" -c user.name=Test -c user.email=test@example.com commit -qm base
check_passes 1.5.0-exp.10
check_passes 1.5.0
check_fails 1.5.0-exp.1 1.5.0-exp.2
check_fails 1.5.0-exp.2 1.5.0-exp.2
check_fails 1.5.0-exp.01 1.5.0-exp.2
check_fails 1.5.0-rc.3 1.5.0-exp.2
check_fails 1.05.0 1.5.0-exp.2

set_version 1.5.0
git -C "$test_root" add Cargo.toml Cargo.lock herdr-plugin.toml CHANGELOG.md
git -C "$test_root" -c user.name=Test -c user.email=test@example.com commit -qm stable
check_fails 1.5.0-exp.11 1.5.0
check_passes 1.5.1-exp.1
set_version 1.4.3
git -C "$test_root" add Cargo.toml Cargo.lock herdr-plugin.toml CHANGELOG.md
git -C "$test_root" -c user.name=Test -c user.email=test@example.com commit -qm main-base
for ref in refs/heads/main refs/remotes/public/main refs/remotes/origin/main refs/heads/exp/ui refs/heads/experimental; do
  git -C "$test_root" update-ref "$ref" HEAD
done
for base in main public/main origin/main refs/heads/main refs/remotes/public/main; do
  set_version 1.5.0-exp.1
  if sh "$test_root/scripts/release.sh" check "$base" > "$test_root/output" 2>&1; then
    printf 'release check accepted experimental version 1.5.0-exp.1 for %s\n' "$base" >&2
    exit 1
  fi
  grep -F 'Choose a stable version for main' "$test_root/output" > /dev/null || {
    cat "$test_root/output" >&2
    exit 1
  }
  set_version 1.5.0
  sh "$test_root/scripts/release.sh" check "$base" > /dev/null
done
set_version 1.5.0-exp.1
for base in exp/ui experimental; do
  sh "$test_root/scripts/release.sh" check "$base" > /dev/null
done

# The installer floor (the U10 review's F3), once main ships install.sh.
commit() {
  git -C "$test_root" add -A
  git -C "$test_root" -c user.name=Test -c user.email=test@example.com commit -qm "$1"
}
set_floor() {
  printf "INSTALLER_FLOOR = '%s'\n" "$1" > "$test_root/scripts/check-update.py"
}
floor_fails() {
  set_version "$1"
  if sh "$test_root/scripts/release.sh" check "$2" > "$test_root/output" 2>&1; then
    printf 'release check accepted INSTALLER_FLOOR %s for %s on %s\n' "$(cat "$test_root/scripts/check-update.py")" "$1" "$2" >&2
    exit 1
  fi
  grep -F "$3" "$test_root/output" > /dev/null || {
    cat "$test_root/output" >&2
    exit 1
  }
}
set_floor 2.0.0
set_version 1.4.3
commit 'a release with no installer'
git -C "$test_root" tag v1.4.3
git -C "$test_root" update-ref refs/heads/main HEAD
set_version 1.4.4
sh "$test_root/scripts/release.sh" check main > /dev/null
printf '#!/bin/sh\n' > "$test_root/scripts/install.sh"
floor_fails 1.5.0 main 'INSTALLER_FLOOR in scripts/check-update.py is 2.0.0, but no release tag v2.0.0 ships scripts/install.sh.'
grep -F 'Set it to 1.5.0 if this is the first release with scripts/install.sh' "$test_root/output" > /dev/null
set_version 1.5.0
for base in exp/ui experimental; do
  sh "$test_root/scripts/release.sh" check "$base" > /dev/null
done
set_version 2.0.0
sh "$test_root/scripts/release.sh" check main > /dev/null
set_floor 1.4.3
floor_fails 1.5.0 origin/main 'no release tag v1.4.3 ships scripts/install.sh'
: > "$test_root/scripts/check-update.py"
floor_fails 1.5.0 main "scripts/check-update.py sets no INSTALLER_FLOOR = 'X.Y.Z' line."
set_floor 1.5.0
set_version 1.5.0
sh "$test_root/scripts/release.sh" check main > /dev/null
commit 'the first release with an installer'
git -C "$test_root" tag v1.5.0
git -C "$test_root" update-ref refs/heads/main HEAD
set_version 1.6.0
sh "$test_root/scripts/release.sh" check refs/remotes/origin/main > /dev/null
set_floor 1.6.0
sh "$test_root/scripts/release.sh" check main > /dev/null
set_floor 1.5.1
floor_fails 1.6.0 main 'no release tag v1.5.1 ships scripts/install.sh'

# runtime.json: on macOS, running processes of the old release read the new one
# through runtime/current, so a live update must not change it (1.12.0's rex_ui).
cp "$(dirname "$0")/../scripts/runtime.py" "$(dirname "$0")/../scripts/update.py" "$test_root/scripts/"
contract() {
  printf '{"runtime_protocol":1,"live_upgrade_epoch":%s,"state_schema":%s,"collaboration_protocol":"collab/v1","command_gate":1,"live_migrate_from":[%s],"helper_gate":1%s}\n' \
    "$1" "$2" "$3" "${4:-}" > "$test_root/runtime.json"
}
set_floor 1.5.0
set_version 1.6.0
contract 1 8 3,4,5,6,7
commit 'a release with runtime.json'
contract_fails() {
  if sh "$test_root/scripts/release.sh" check "$1" > "$test_root/output" 2>&1; then
    printf 'release check accepted runtime.json %s against %s\n' "$(cat "$test_root/runtime.json" 2>&1)" "$1" >&2
    exit 1
  fi
  grep -F "$2" "$test_root/output" > /dev/null || {
    cat "$test_root/output" >&2
    exit 1
  }
}
# A runtime.json it cannot read fails closed on one line that names the problem.
contract_unreadable() {
  contract_fails main "$1"
  [ "$(wc -l < "$test_root/output")" -eq 1 ] || {
    cat "$test_root/output" >&2
    exit 1
  }
}
git -C "$test_root" update-ref refs/heads/main HEAD
git -C "$test_root" update-ref refs/heads/experimental HEAD
set_version 1.6.1
sh "$test_root/scripts/release.sh" check main > /dev/null
contract 1 8 3,4,5,6,7 ',"rex_ui":1'
contract_fails main 'On macOS, running processes of the old release read the new runtime.json through runtime/current'
grep -F 'Restore runtime.json from main byte for byte' "$test_root/output" > /dev/null
grep -F 'It needs activation to stop or hand off old Channels servers first, and that is not built yet.' "$test_root/output" > /dev/null
contract_fails experimental 'runtime.json differs from the one at experimental'
contract 1 8 3,4,5,6
contract_fails main 'and the update would still run live.'
# An epoch increase passes, though on macOS old Channels servers outlive it.
contract 2 8 3,4,5,6,7 ',"rex_ui":1'
sh "$test_root/scripts/release.sh" check main > /dev/null
contract 1 9 3,4,5,6,7,8
sh "$test_root/scripts/release.sh" check main > /dev/null
printf '{"runtime_protocol":1,' > "$test_root/runtime.json"
contract_unreadable 'runtime.json is not valid JSON: '
printf '[1]\n' > "$test_root/runtime.json"
contract_unreadable 'runtime.json is not a JSON object.'
printf '{"runtime_protocol":1}\n' > "$test_root/runtime.json"
contract_unreadable 'runtime.json has no live_upgrade_epoch.'
rm "$test_root/runtime.json"
contract_unreadable 'runtime.json is missing. Restore it from main.'
printf '%s\n' 'release version checks passed'
