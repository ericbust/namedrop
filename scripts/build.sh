#!/bin/sh
set -eu
# Herdr runs this build command while installing the plugin, before it is
# registered, and without workspace, pane or socket context. It must not
# activate Namedrop, contact the running host, change the manifest Herdr
# previewed, or install a missing toolchain. A nonzero exit aborts installation,
# so a failed build can never be reported as an installed plugin.
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$namedrop_root"

case "$(uname -s) $(uname -m)" in
  'Darwin arm64') target=aarch64-apple-darwin ;;
  'Linux x86_64') target=x86_64-unknown-linux-gnu ;;
  'Linux aarch64') target=aarch64-unknown-linux-gnu ;;
  *)
    printf '%s\n' \
      "Namedrop supports macOS on Apple silicon and GNU/Linux on x86_64 or aarch64; this machine reports $(uname -s) $(uname -m)." \
      'On a Mac, build from a native arm64 terminal rather than an Intel-emulated one.' >&2
    exit 69 ;;
esac
# Cargo itself refuses an older toolchain, because Cargo.toml declares
# rust-version; this only reports the case where there is no cargo at all.
if ! command -v cargo >/dev/null 2>&1; then
  printf '%s\n' \
    'Building Namedrop from source needs Rust 1.97 or newer, and cargo is not on PATH.' \
    'Install Rust from https://rustup.rs, open a new terminal so PATH is refreshed, then install Namedrop again.' \
    'Namedrop never installs a toolchain for you.' >&2
  exit 69
fi
if [ "$target" = aarch64-apple-darwin ]; then
  if command -v xcode-select >/dev/null 2>&1 && ! xcode-select -p >/dev/null 2>&1; then
    printf '%s\n' \
      'Namedrop bundles SQLite, so building it needs Apple command-line developer tools.' \
      'Run xcode-select --install, wait for it to finish, then install Namedrop again.' >&2
    exit 69
  fi
elif ! getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
  printf '%s\n' \
    'Namedrop Linux builds target the GNU C library (glibc); this system does not report one.' \
    'musl-based distributions such as Alpine are not supported.' >&2
  exit 69
elif ! command -v cc >/dev/null 2>&1; then
  printf '%s\n' \
    'Namedrop bundles SQLite, so building it needs a C compiler and linker on PATH as cc.' \
    'Install your distribution build tools (Arch: base-devel, Debian/Ubuntu: build-essential, Fedora: gcc), then install Namedrop again.' >&2
  exit 69
fi

# Build inside this checkout, where older Mac fallbacks look, even when the
# caller has configured a different Cargo target directory.
cargo build --locked --release --target "$target" --target-dir "$namedrop_root/target"
# Stage the result at bin/namedrop, the path run.sh, activation and the updater
# check first. Renaming a complete copy never writes into an executable that a
# running command is using, which Linux refuses as a busy text file.
mkdir -p bin
trap 'rm -f "bin/.namedrop.$$"' EXIT
cp -p "target/$target/release/namedrop" "bin/.namedrop.$$"
mv -f "bin/.namedrop.$$" bin/namedrop
