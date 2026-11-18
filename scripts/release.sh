#!/bin/sh
set -eu
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
usage() {
  printf '%s\n' 'Usage: sh scripts/release.sh version | check BASE-REF | notes VERSION' \
    'version  prints the release version after checking Cargo.toml, Cargo.lock and herdr-plugin.toml agree.' \
    'check    fails unless the version is above the one at BASE-REF and CHANGELOG.md has a section for it,' \
    '         and, for main, unless INSTALLER_FLOOR names a release that ships scripts/install.sh.' \
    '         It also fails when runtime.json differs from the one at BASE-REF in an update that runs live.' \
    'notes    prints the CHANGELOG.md section for VERSION without its heading.'
}
fail() { printf '%s\n' "$@" >&2; exit 65; }
package_version() { awk -F '"' '/^version = / {print $2; exit}'; }
lock_version() { awk -F '"' '$0 == "name = \"namedrop\"" {found=1; next} found && /^version = / {print $2; exit}'; }

release_version() {
  version=$(package_version < "$namedrop_root/Cargo.toml")
  valid_version "$version" ||
    fail "Cargo.toml version '$version' is not MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-exp.N."
  manifest=$(package_version < "$namedrop_root/herdr-plugin.toml")
  [ "$manifest" = "$version" ] ||
    fail "herdr-plugin.toml has version '$manifest' but Cargo.toml has '$version'. Set the same version in both."
  lock=$(lock_version < "$namedrop_root/Cargo.lock")
  [ "$lock" = "$version" ] ||
    fail "Cargo.lock has namedrop version '$lock' but Cargo.toml has '$version'. Run 'cargo update --workspace' and commit Cargo.lock."
  printf '%s\n' "$version"
}

valid_version() {
  printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-exp\.(0|[1-9][0-9]*))?$'
}

# Prints 1 when $1 is a higher release version than $2, otherwise 0.
version_above() {
  printf '%s %s\n' "$1" "$2" | awk '{
    split($1, left, "-exp\\."); split($2, right, "-exp\\.")
    split(left[1], a, "."); split(right[1], b, ".")
    for (i = 1; i <= 3; i++) {
      if (length(a[i]) != length(b[i])) { print (length(a[i]) > length(b[i]) ? 1 : 0); exit }
      if (("x" a[i]) != ("x" b[i])) { print (("x" a[i]) > ("x" b[i]) ? 1 : 0); exit }
    }
    if (left[2] == "" || right[2] == "") { print (right[2] != "" ? 1 : 0); exit }
    if (length(left[2]) != length(right[2])) { print (length(left[2]) > length(right[2]) ? 1 : 0); exit }
    print (left[2] != right[2] && ("x" left[2]) > ("x" right[2]) ? 1 : 0)
  }'
}

# A standalone install is offered no release below scripts/check-update.py's
# INSTALLER_FLOOR, because an older release has no installer. Once main ships
# scripts/install.sh, the floor must be this release, or a tagged release that
# ships scripts/install.sh: a forgotten floor offers nothing, and one set too
# low offers releases that cannot be installed.
installer_floor() {
  [ "${1##*/}" = main ] && [ -f "$namedrop_root/scripts/install.sh" ] || return 0
  floor=$(sed -n "s/^INSTALLER_FLOOR = '\(.*\)'\$/\1/p" "$namedrop_root/scripts/check-update.py")
  [ -n "$floor" ] || fail "scripts/check-update.py sets no INSTALLER_FLOOR = 'X.Y.Z' line."
  [ "$floor" = "$2" ] || git -C "$namedrop_root" cat-file -e "v$floor:scripts/install.sh" 2>/dev/null ||
    fail "INSTALLER_FLOOR in scripts/check-update.py is $floor, but no release tag v$floor ships scripts/install.sh." \
      "Set it to $2 if this is the first release with scripts/install.sh, else to the first release that shipped it, and fetch tags first (see docs/releasing.md#publish)."
}

# A running Namedrop process, a Channels server or a helper, reads the new
# release's runtime.json through runtime/current after a live update, and
# stops working when it differs from its own copy. So runtime.json changes
# only in an update that already expects old processes to stop: one that
# cannot run live, or one that migrates the store (scripts/runtime.py).
runtime_contract() {
  git -C "$namedrop_root" cat-file -e "$1:runtime.json" 2>/dev/null || return 0
  git -C "$namedrop_root" show "$1:runtime.json" | cmp -s - "$namedrop_root/runtime.json" && return 0
  git -C "$namedrop_root" show "$1:runtime.json" | python3 -I -c '
import json, sys
sys.path.insert(0, sys.argv[1] + "/scripts")
from runtime import live_compatible
previous, incoming = json.load(sys.stdin), json.load(open(sys.argv[1] + "/runtime.json"))
sys.exit(0 if not live_compatible(previous, incoming) or previous["state_schema"] != incoming["state_schema"] else 1)
' "$namedrop_root" ||
    fail "runtime.json differs from the one at $1, and the update would still run live." \
      "Running processes of the old release read the new runtime.json through runtime/current and stop working when it differs from their own copy." \
      "Restore runtime.json from $1 byte for byte, and let the binary answer a new capability with its own query beside --runtime-info in src/main.rs." \
      "Change runtime.json only in an update that cannot run live, such as a live_upgrade_epoch increase, or with a state_schema migration (see docs/releasing.md#live-update-compatibility)."
}

section() {
  awk -v heading="## [$1] - " '
    index($0, heading) == 1 && substr($0, length(heading) + 1) ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/ {found=1; next}
    found && /^## / {exit}
    found {lines[++n] = $0}
    END {
      if (!found) exit 1
      first = 1; while (first <= n && lines[first] == "") first++
      last = n; while (last >= first && lines[last] == "") last--
      for (i = first; i <= last; i++) print lines[i]
    }
  ' "$namedrop_root/CHANGELOG.md"
}

[ "$#" -ge 1 ] || { usage >&2; exit 64; }
case "$1" in
  version)
    [ "$#" -eq 1 ] || { usage >&2; exit 64; }
    release_version ;;
  check)
    [ "$#" -eq 2 ] || { usage >&2; exit 64; }
    base=$2
    version=$(release_version)
    case "${base##*/}:$version" in
      main:*-exp.*)
        fail "Experimental version $version cannot be released to main. Choose a stable version for main in Cargo.toml, herdr-plugin.toml and Cargo.lock." ;;
    esac
    base_toml=$(git -C "$namedrop_root" show "$base:Cargo.toml" 2>/dev/null) ||
      fail "Can't read Cargo.toml at '$base'. Fetch the base branch with full history first."
    base_version=$(printf '%s\n' "$base_toml" | package_version)
    valid_version "$base_version" || fail "Cargo.toml at '$base' has invalid version '$base_version'."
    runtime_contract "$base"
    [ "$(version_above "$version" "$base_version")" = 1 ] ||
      fail "Version $version is not above $base_version on $base. Every merge to main is a release." \
        "Raise the version in Cargo.toml, herdr-plugin.toml and Cargo.lock (see docs/releasing.md#choose-the-version)." \
        "If this pull request changes nothing users run, label it no-release instead."
    section "$version" > /dev/null ||
      fail "CHANGELOG.md has no '## [$version] - YYYY-MM-DD' section. Add one at the top describing what changes for users."
    installer_floor "$base" "$version"
    printf '%s\n' "Release $version is ready: above $base_version on $base, with a changelog section." ;;
  notes)
    [ "$#" -eq 2 ] || { usage >&2; exit 64; }
    section "$2" || fail "CHANGELOG.md has no '## [$2] - YYYY-MM-DD' section." ;;
  -h|--help) usage ;;
  *) usage >&2; exit 64 ;;
esac
