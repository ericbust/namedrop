#!/bin/sh
set -eu
case " $* " in
  *' --help '*|*' -h '*) printf '%s\n' 'Usage: sh scripts/update.sh [--open]' 'Open the Update Namedrop tab with --open; otherwise run the interactive guided update.'; exit 0 ;;
esac
command -v python3 >/dev/null 2>&1 || { printf '%s\n' 'Update Namedrop requires Python 3. Install it, then reopen this tab.' >&2; exit 69; }
# exec leaves no shell waiting to read this checkout after native replacement.
exec python3 "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)/update.py" "$@"
