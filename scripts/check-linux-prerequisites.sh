#!/bin/sh
# Read-only Linux preflight for Namedrop acceptance. It installs nothing, uses
# no sudo, starts no agent session and reads no credential contents: sign-in
# files are reported only as present or absent. Paste the output into the
# acceptance record; exit status 1 means a required prerequisite is missing.
set -u
missing=

row() { printf '%-22s %s\n' "$1:" "$2"; }

# probe required|optional LABEL COMMAND [ARGS...]: first output line and path.
probe() {
  need=$1 label=$2
  shift 2
  if command -v "$1" >/dev/null 2>&1; then
    row "$label" "$("$@" 2>&1 | head -n 1) [$(command -v "$1")]"
  elif [ "$need" = optional ]; then
    row "$label" "not found"
  else
    row "$label" "missing (required)"
    missing="$missing $label"
  fi
}

# at_least VERSION MAJOR MINOR: succeeds when the first X.Y in VERSION is new enough.
at_least() {
  printf '%s\n' "$1" | awk -v want_major="$2" -v want_minor="$3" '
    match($0, /[0-9]+\.[0-9]+/) {
      split(substr($0, RSTART, RLENGTH), v, ".")
      found = 1
      exit !(v[1] > want_major || (v[1] == want_major && v[2] >= want_minor))
    }
    END { if (!found) exit 1 }'
}

os_field() { sed -n "s/^$1=//p" /etc/os-release 2>/dev/null | tr -d '"'; }

if [ "$(uname -s)" != Linux ]; then
  printf 'This preflight is for Linux; this machine reports %s.\n' "$(uname -s)" >&2
  exit 2
fi

printf '%s\n' 'Namedrop Linux preflight (read-only)'
row date "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
row distribution "$(os_field PRETTY_NAME) (ID=$(os_field ID), VERSION_ID=$(os_field VERSION_ID), BUILD_ID=$(os_field BUILD_ID))"
probe optional omarchy omarchy-version
machine=$(uname -m)
case "$machine" in
  x86_64) row architecture "x86_64 -> x86_64-unknown-linux-gnu" ;;
  aarch64) row architecture "aarch64 -> aarch64-unknown-linux-gnu" ;;
  *) row architecture "$machine (unsupported)"; missing="$missing architecture" ;;
esac
row kernel "$(uname -r)"
if libc=$(getconf GNU_LIBC_VERSION 2>/dev/null); then
  row libc "$libc"
else
  row libc "not glibc (unsupported)"
  missing="$missing glibc"
fi
row "security modules" "$(cat /sys/kernel/security/lsm 2>/dev/null || printf unreadable)"
# Codex helpers need Landlock ABI 1 or newer. The version query
# landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) restricts
# nothing; it is syscall 444 on x86_64 and aarch64 only, so skip it elsewhere.
case "$machine" in
  x86_64|aarch64)
    landlock=$(python3 -c '
import ctypes, os
libc = ctypes.CDLL(None, use_errno=True)
abi = libc.syscall(444, None, ctypes.c_size_t(0), ctypes.c_uint32(1))
print(abi if abi >= 0 else "unavailable (" + os.strerror(ctypes.get_errno()) + ")")' 2>/dev/null) || landlock='not checked' ;;
  *) landlock='not checked (unsupported architecture)' ;;
esac
row "landlock abi" "${landlock:-not checked}"
case "$landlock" in
  [1-9]|[1-9][0-9]) ;;
  *) missing="$missing landlock-abi>=1" ;;
esac

row "login shell" "${SHELL:-unset}"
probe optional bash bash --version
probe optional zsh zsh --version
row terminal "TERM=${TERM:-unset} TERM_PROGRAM=${TERM_PROGRAM:-unset}"
row "inside herdr" "$([ -n "${HERDR_ENV-}" ] && printf yes || printf no)"

probe required herdr herdr --version
probe required python3 python3 --version
if command -v python3 >/dev/null 2>&1 && ! at_least "$(python3 --version 2>&1)" 3 9; then
  missing="$missing python3>=3.9"
fi
probe required git git --version
probe required codex codex --version
probe required claude claude --version
probe optional namedrop namedrop --version
# Linux Codex helpers execute only a static native Codex binary: the command
# itself, or the musl binary inside the official @openai/codex npm layout.
# Informational; Namedrop Doctor is authoritative.
if command -v codex >/dev/null 2>&1 && command -v file >/dev/null 2>&1; then
  codex_entry=$(readlink -f "$(command -v codex)")
  codex_kind=$(file -b "$codex_entry")
  row "codex entrypoint" "$codex_entry: $codex_kind"
  case "$codex_kind" in
    *ELF*static*) row "codex native binary" "the entrypoint itself (static)" ;;
    *JavaScript*|*script*)
      case "$codex_entry" in
        */@openai/codex/bin/*) ;;
        *) row "codex native binary" "unrecognized wrapper; Doctor will report the requirement"; codex_entry= ;;
      esac
      if [ -n "$codex_entry" ]; then
      codex_native=$(find "$(dirname "$(dirname "$codex_entry")")/.." -maxdepth 5 -type f \
        -path "*/vendor/$machine-unknown-linux-musl/*/codex" 2>/dev/null | head -n 1)
      row "codex native binary" "${codex_native:-not found}${codex_native:+: $(file -b "$codex_native")}"
      fi ;;
    *) row "codex native binary" "unsupported entrypoint; install the official static Codex build" ;;
  esac
fi

# Source installs only; the prebuilt archive needs none of these.
probe optional cargo cargo --version
probe optional rustc rustc --version
if command -v rustc >/dev/null 2>&1 && ! at_least "$(rustc --version 2>&1)" 1 97; then
  row "rust note" "source installs need Rust 1.97 or newer"
fi
probe optional cc cc --version
# The Landlock test canary links with cc -static, which needs glibc's libc.a.
if command -v cc >/dev/null 2>&1; then
  static=$(cc -print-file-name=libc.a 2>/dev/null)
  case "$static" in /*) [ -f "$static" ] && row "static libc" "$static" || row "static libc" "not found (cc -static tests fail)" ;;
    *) row "static libc" "not found (cc -static tests fail)" ;; esac
fi
# Archive verification.
probe optional sha256sum sha256sum --version
probe optional file file --version
probe optional tar tar --version

present() { if [ -e "$1" ]; then printf present; else printf absent; fi; }
set_or_unset() { if [ -n "$1" ]; then printf set; else printf unset; fi; }
row "codex sign-in file" "$(present "${CODEX_HOME:-$HOME/.codex}/auth.json") (presence only; keyring sign-ins show absent)"
row "claude sign-in file" "$(present "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json") (presence only)"
row "api key variables" "OPENAI_API_KEY $(set_or_unset "${OPENAI_API_KEY-}"), ANTHROPIC_API_KEY $(set_or_unset "${ANTHROPIC_API_KEY-}")"

if [ -n "$missing" ]; then
  row result "missing required:$missing"
  exit 1
fi
row result "required prerequisites present; authentication is verified only by the acceptance run"
