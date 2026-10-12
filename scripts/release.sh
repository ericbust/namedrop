#!/bin/sh
set -eu
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
usage() {
  printf '%s\n' 'Usage: sh scripts/release.sh version | check BASE-REF | notes VERSION' \
    'version  prints the release version after checking Cargo.toml, Cargo.lock and herdr-plugin.toml agree.' \
    'check    fails unless the version is above the one at BASE-REF and CHANGELOG.md has a section for it.' \
    'notes    prints the CHANGELOG.md section for VERSION without its heading.'
}
fail() { printf '%s\n' "$@" >&2; exit 65; }
package_version() { awk -F '"' '/^version = / {print $2; exit}'; }
lock_version() { awk -F '"' '$0 == "name = \"namedrop\"" {found=1; next} found && /^version = / {print $2; exit}'; }

release_version() {
  version=$(package_version < "$namedrop_root/Cargo.toml")
  printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    fail "Cargo.toml version '$version' is not MAJOR.MINOR.PATCH."
  manifest=$(package_version < "$namedrop_root/herdr-plugin.toml")
  [ "$manifest" = "$version" ] ||
    fail "herdr-plugin.toml has version '$manifest' but Cargo.toml has '$version'. Set the same version in both."
  lock=$(lock_version < "$namedrop_root/Cargo.lock")
  [ "$lock" = "$version" ] ||
    fail "Cargo.lock has namedrop version '$lock' but Cargo.toml has '$version'. Run 'cargo update --workspace' and commit Cargo.lock."
  printf '%s\n' "$version"
}

# Prints 1 when $1 is a higher MAJOR.MINOR.PATCH than $2, otherwise 0.
version_above() {
  printf '%s %s\n' "$1" "$2" | awk '{
    split($1, a, "."); split($2, b, ".")
    for (i = 1; i <= 3; i++) if (a[i] + 0 != b[i] + 0) { print (a[i] + 0 > b[i] + 0) ? 1 : 0; exit }
    print 0
  }'
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
    base_toml=$(git -C "$namedrop_root" show "$base:Cargo.toml" 2>/dev/null) ||
      fail "Can't read Cargo.toml at '$base'. Fetch the base branch with full history first."
    base_version=$(printf '%s\n' "$base_toml" | package_version)
    [ "$(version_above "$version" "$base_version")" = 1 ] ||
      fail "Version $version is not above $base_version on $base. Every merge to main is a release." \
        "Raise the version in Cargo.toml, herdr-plugin.toml and Cargo.lock (see docs/releasing.md#choose-the-version)." \
        "If this pull request changes nothing users run, label it no-release instead."
    section "$version" > /dev/null ||
      fail "CHANGELOG.md has no '## [$version] - YYYY-MM-DD' section. Add one at the top describing what changes for users."
    printf '%s\n' "Release $version is ready: above $base_version on $base, with a changelog section." ;;
  notes)
    [ "$#" -eq 2 ] || { usage >&2; exit 64; }
    section "$2" || fail "CHANGELOG.md has no '## [$2] - YYYY-MM-DD' section." ;;
  -h|--help) usage ;;
  *) usage >&2; exit 64 ;;
esac
