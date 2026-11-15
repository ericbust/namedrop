#!/bin/sh
set -eu
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
yes=no
while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes) yes=yes ;;
    -h|--help) printf '%s\n' 'Usage: sh scripts/install.sh [--yes]' 'Install Namedrop for Rex, or adopt the existing Herdr runtime.'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 64 ;;
  esac
  shift
done
case "$(uname -s) $(uname -m)" in
  'Darwin arm64'|'Linux x86_64'|'Linux aarch64') ;;
  *)
    printf '%s\n' \
      "Namedrop supports macOS on Apple silicon and GNU/Linux on x86_64 or aarch64; this machine reports $(uname -s) $(uname -m)." \
      'On a Mac, build from a native arm64 terminal rather than an Intel-emulated one.' >&2
    exit 69 ;;
esac
if ! python3 -c 'import sys' >/dev/null 2>&1; then
  printf '%s\n' 'Namedrop needs Python 3. On macOS, install the Command Line Tools with xcode-select --install, then run sh scripts/install.sh again.' >&2
  exit 69
fi
if ! command -v rex >/dev/null 2>&1; then
  if [ -n "${REX_BIN_DIR:-}" ] && [ -f "$REX_BIN_DIR/rex" ] && [ -x "$REX_BIN_DIR/rex" ]; then
    PATH="$REX_BIN_DIR:$PATH"
    export PATH
  else
    printf '%s\n' 'Namedrop needs Rex. Put rex on PATH, or set REX_BIN_DIR to the directory containing rex, then run sh scripts/install.sh again.' >&2
    exit 69
  fi
fi
# Imports must not create config or cache files before confirmation.
export PYTHONDONTWRITEBYTECODE=1
python3 - "$namedrop_root" "$yes" 3<&0 <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys


def require(condition, message):
    if not condition:
        raise ValueError(message)


def owner(state):
    runtime = state / 'runtime'
    path = runtime / 'owner'
    require(not path.is_symlink(), f'Runtime file has an unexpected type: {path}')
    value = path.read_text().strip() if path.exists() else 'herdr' if (runtime / 'current').exists() else None
    require(value in (None, 'standalone', 'herdr'), f'Invalid Namedrop runtime owner in {state}')
    return value


try:
    root, yes = sys.argv[1:]
    home = Path(os.environ['HOME'])
    require(home.is_absolute(), 'Namedrop install needs an absolute HOME.')
    record = home / '.config/namedrop/state-directory'
    state = None
    if record.exists():
        recorded = Path(record.read_text().strip())
        launcher = recorded / 'runtime/namedrop'
        if recorded.is_absolute() and launcher.is_file() and os.access(launcher, os.X_OK):
            state = recorded
    recorded = state is not None
    if state is None:
        state = Path(os.environ.get('NAMEDROP_STATE_DIR') or
                     str(Path(os.environ.get('XDG_STATE_HOME') or str(home / '.local/state')) / 'namedrop'))
    require(state.is_absolute(), 'Namedrop install needs an absolute state directory.')
    retired = state / 'runtime/retired.json'
    if retired.exists():
        reason = json.loads(retired.read_text()).get('reason')
        require(reason != 'herdr_handoff', 'Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.')
        require(reason == 'removed', f'Invalid Namedrop retirement marker in {state}')
    selected_owner = owner(state)
    require(recorded or selected_owner != 'herdr',
            "ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in "
            f'{state}. Run Activate Namedrop in Herdr to update them.')
    launcher = state / 'runtime/namedrop'
    decision = 'adopt' if recorded and selected_owner == 'herdr' else 'update' if recorded else 'new'
    if decision == 'adopt':
        result = subprocess.run([str(launcher), '--runtime-info'], capture_output=True, text=True)
        try:
            supported = result.returncode == 0 and json.loads(result.stdout).get('rex_ui') == 1
        except (ValueError, AttributeError):
            supported = False
        require(supported, 'Update Namedrop in Herdr first, then run sh scripts/install.sh again.')
    print(f'Namedrop Rex install: {decision}\nState directory: {state}\nLauncher: {launcher}')
    print('Files to install or update:')
    if decision != 'adopt':
        print(f'  {state}/runtime (releases, current, owner, source.json and migration records)')
    print(f'  {record}\n  {home}/.config/namedrop/rex.json')
    print('  namedrop.lua and the Namedrop line in init.lua beside the running Rex server\'s config')
    print(f'  {home}/.local/bin/namedrop')
    print(f'  Agent packages and permissions under {os.environ.get("CLAUDE_CONFIG_DIR") or home / ".claude"}')
    print(f'  Agent packages, rules, config and sandbox root under {os.environ.get("CODEX_HOME") or home / ".codex"}')
    if decision == 'adopt':
        print("Herdr keeps its runtime and updates it. This archive publishes no runtime.")
    if yes != 'yes':
        print('Install Namedrop for Rex? [y/N] ', end='', flush=True)
        with os.fdopen(3) as answer:
            confirmed = answer.readline().strip().lower() in ('y', 'yes')
        if not confirmed:
            print('Installation cancelled; no files changed.')
            sys.exit(0)
    if decision == 'adopt':
        command = [str(launcher), '--plugin-state', str(state), '--json', 'install',
                   '--input-json', '{"confirmed":true}']
    else:
        state.mkdir(parents=True, exist_ok=True)
        command = [sys.executable, str(Path(root) / 'scripts/runtime.py'), root,
                   '--state-dir', str(state), '--verify', 'install']
    result = subprocess.run(command, capture_output=True, text=True)
    if result.stderr:
        print(result.stderr, end='', file=sys.stderr)
    if result.returncode:
        if result.stdout:
            print(result.stdout, end='', file=sys.stderr)
        sys.exit(result.returncode)
    report = json.loads(result.stdout)
    require(report.get('status') == 'ok' and report.get('operation') == 'install',
            'Namedrop did not return an installation report. Inspect setup before retrying.')
    data = report['data']
    print(f'Module: {data["module"]}\nConfig: {data["config"]}\ninit.lua line: {data["line"]}')
    print('Keys in effect: ' + json.dumps(data['keys'], ensure_ascii=False))
    if data.get('manual_config'):
        print(data['instruction'])
    guidance = data.get('agent_integration_hook_activation')
    if guidance:
        print('Agent hook approval: ' + json.dumps(guidance, ensure_ascii=False))
    print('In the Rex palette, run Activate Namedrop, then Name this pane. Start new agent sessions.')
except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
    print(f'Namedrop Rex installation stopped: {error}', file=sys.stderr)
    sys.exit(2)
PY
