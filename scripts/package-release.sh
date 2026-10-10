#!/bin/sh
set -eu
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
output="$namedrop_root/target/dist"
binary=
target=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output|--binary|--target)
      option=$1
      [ "$#" -ge 2 ] || { printf 'Missing value for %s\n' "$option" >&2; exit 64; }
      case "$option" in --output) output=$2;; --binary) binary=$2;; --target) target=$2;; esac
      shift 2 ;;
    -h|--help) printf '%s\n' 'Usage: sh scripts/package-release.sh [--target TRIPLE] [--output DIRECTORY] [--binary EXECUTABLE]' 'TRIPLE is aarch64-apple-darwin, x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu; it defaults to this machine.' 'Builds in target/release-build by default; an existing binary must match the target and package version, so package on the target machine.'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 64 ;;
  esac
done
if [ -z "$target" ]; then
  case "$(uname -s) $(uname -m)" in
    'Darwin arm64') target=aarch64-apple-darwin ;;
    'Linux x86_64') target=x86_64-unknown-linux-gnu ;;
    'Linux aarch64') target=aarch64-unknown-linux-gnu ;;
  esac
fi
# `file` identifies the executable format. A GNU/Linux archive must also use
# glibc's dynamic loader for that architecture, not musl's or a Nix store path.
case "$target" in
  aarch64-apple-darwin) platform=macos format='*Mach-O*arm64*' ;;
  x86_64-unknown-linux-gnu) platform=linux format='ELF 64-bit LSB *, x86-64,*interpreter /lib64/ld-linux-x86-64.so.2*' ;;
  aarch64-unknown-linux-gnu) platform=linux format='ELF 64-bit LSB *, ARM aarch64,*interpreter /lib/ld-linux-aarch64.so.1*' ;;
  *) printf '%s\n' "Unsupported release target '$target'. Choose aarch64-apple-darwin, x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu." >&2; exit 64 ;;
esac
version=$(awk -F '"' '/^version = / {print $2; exit}' "$namedrop_root/Cargo.toml")
manifest_version=$(awk -F '"' '/^version = / {print $2; exit}' "$namedrop_root/herdr-plugin.toml")
[ -n "$version" ] && [ "$version" = "$manifest_version" ] || { printf '%s\n' 'Cargo and Herdr manifest versions must match.' >&2; exit 65; }
name="namedrop-$version-$target"
mkdir -p "$output"
output=$(CDPATH= cd -- "$output" && pwd -P)
archive="$output/$name.tar.gz"
checksum="$archive.sha256"
if [ -e "$archive" ] || [ -e "$checksum" ]; then
  printf '%s\n' 'Release output already exists. Choose a fresh --output directory; published artifacts are never overwritten.' >&2
  exit 73
fi
if [ -z "$binary" ]; then
  cargo build --manifest-path "$namedrop_root/Cargo.toml" --locked --release --target "$target" --target-dir "$namedrop_root/target/release-build"
  binary="$namedrop_root/target/release-build/$target/release/namedrop"
fi
[ -x "$binary" ] || { printf '%s\n' 'The release binary must exist and be executable.' >&2; exit 65; }
description=$(file -b "$binary" || :)
# shellcheck disable=SC2254 # Each format is deliberately an unquoted glob.
case "$description" in
  $format) ;;
  *) printf '%s\n' "The release binary is not an executable for $target: $description" >&2; exit 65 ;;
esac
# Unlike a shell, execv never reinterprets a foreign executable as a script.
reported=$(python3 -c 'import os, sys; os.execv(sys.argv[1], sys.argv[1:])' "$binary" --version 2>/dev/null || :)
[ "$reported" = "namedrop $version" ] || { printf '%s\n' 'The release binary does not run on this machine or its version does not match Cargo.toml. Package each target on a machine of that target.' >&2; exit 65; }
# GNU/Linux reliably has coreutils' sha256sum and macOS has shasum; both print
# the same lines and verify them with -c.
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
stage=$(mktemp -d "$output/.namedrop-stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
package="$stage/$name"
mkdir -p "$package/bin" "$package/scripts" "$package/docs"
cp "$binary" "$package/bin/namedrop"
cp "$namedrop_root/README.md" "$namedrop_root/CONTRIBUTING.md" "$namedrop_root/LICENSE" "$package/"
cp "$namedrop_root/scripts/setup.sh" "$package/scripts/"
cp "$namedrop_root/scripts/update.sh" "$namedrop_root/scripts/update.py" "$package/scripts/"
cp "$namedrop_root/scripts/run.sh" "$namedrop_root/scripts/runtime.py" "$package/scripts/"
cp "$namedrop_root/scripts/check-linux-prerequisites.sh" "$package/scripts/"
cp "$namedrop_root/runtime.json" "$package/"
for doc in operations compatibility releasing; do
  cp "$namedrop_root/docs/$doc.md" "$package/docs/"
done
chmod 755 "$package/bin/namedrop" "$package/scripts/setup.sh" "$package/scripts/update.sh"
# Keep one manifest source. A released package has no build step, runs the
# bundled bin/namedrop through run.sh, and declares only its own platform.
awk -v platform="$platform" '
  /^\[\[build\]\]/ {skip=1; next}
  skip && /^\[/ {skip=0}
  skip {next}
  /^platforms = / {print "platforms = [\"" platform "\"]"; next}
  {print}
' "$namedrop_root/herdr-plugin.toml" > "$package/herdr-plugin.toml"
(
  cd "$package"
  sha256 bin/namedrop herdr-plugin.toml runtime.json README.md CONTRIBUTING.md LICENSE scripts/setup.sh scripts/update.sh scripts/update.py scripts/run.sh scripts/runtime.py scripts/check-linux-prerequisites.sh docs/*.md > SHA256SUMS
)
COPYFILE_DISABLE=1 tar -czf "$stage/$name.tar.gz" -C "$stage" "$name"
(
  cd "$stage"
  sha256 "$name.tar.gz" > "$name.tar.gz.sha256"
)
# Hard links publish complete files without clobbering a concurrent build.
ln "$stage/$name.tar.gz" "$archive"
ln "$stage/$name.tar.gz.sha256" "$checksum"
printf '%s\n' "$archive" "$checksum"
