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
printf '%s\n' 'release version checks passed'
