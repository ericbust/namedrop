#!/bin/sh
set -eu
# Imports must not create config or cache files before confirmation. The
# Python calls here also pass -B, because -I ignores this variable.
export PYTHONDONTWRITEBYTECODE=1
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
platform="$(uname -s 2>/dev/null || :) $(uname -m 2>/dev/null || :)"
case "$platform" in
  'Darwin arm64'|'Linux x86_64'|'Linux aarch64') ;;
  *)
    [ "$platform" = ' ' ] && platform='no platform (uname did not run)'
    printf '%s\n' \
      "Namedrop supports macOS on Apple silicon and GNU/Linux on x86_64 or aarch64; this machine reports $platform." \
      'On a Mac, build from a native arm64 terminal rather than an Intel-emulated one.' >&2
    exit 69 ;;
esac
if ! python3 -B -I -c 'import sys' >/dev/null 2>&1; then
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
# The answer is read from fd 3. A closed stdin answers no.
if (exec 9<&0) 2>/dev/null; then exec 3<&0; else exec 3</dev/null; fi
python3 -B -I - "$namedrop_root" "$yes" <<'PY'
import json
import os
from pathlib import Path
import stat
import subprocess
import sys


def require(condition, message):
    if not condition:
        raise ValueError(message)


def owner(state, which='this Namedrop state'):
    runtime = state / 'runtime'
    path = runtime / 'owner'
    require(not path.is_symlink(), f'Runtime file has an unexpected type: {path}')
    try:
        value = path.read_text().strip() if path.exists() else 'herdr' if (runtime / 'current').exists() else None
    except (OSError, UnicodeDecodeError):
        raise ValueError(f'Namedrop cannot read the owner of {which} in {path}: it is not a readable file. '
                         'Inspect that file, then run sh scripts/install.sh again.') from None
    require(value in (None, 'standalone', 'herdr'), f'Invalid Namedrop runtime owner in {state}')
    return value


def herdr_lists_namedrop(home, herdr):
    """Whether Herdr's plugin registry still lists Namedrop. Herdr keeps it in
    $XDG_CONFIG_HOME/herdr/plugins.json whenever that is set, even empty or
    relative, else in ~/.config/herdr/plugins.json (herdr's
    config::config_dir), and reads a missing file as no plugins."""
    config = os.environ.get('XDG_CONFIG_HOME')
    registry = (home / '.config' if config is None else Path(config)) / 'herdr/plugins.json'
    # Herdr reads a relative directory from its own working directory, which
    # may not be this one, so a list missing here says nothing.
    relative = not registry.is_absolute()
    try:
        registry = registry.absolute()
        plugins = json.loads(registry.read_bytes())
    except FileNotFoundError:
        if not relative:
            return False
        plugins = None
    except (OSError, ValueError):
        plugins = None
    require(isinstance(plugins, list),
            f"Namedrop cannot read Herdr's plugin list in {registry}, so it cannot tell whether "
            f'Namedrop is still installed in Herdr ({herdr}). If it is not, move that directory '
            'away, then run sh scripts/install.sh again.')
    return any(isinstance(plugin, dict) and plugin.get('plugin_id') == 'namedrop' for plugin in plugins)


def supports_rex(launcher):
    """Whether the runtime behind `launcher` installs the Rex module: its
    contract names rex_ui, from U7 on."""
    try:
        result = subprocess.run([str(launcher), '--runtime-info'], capture_output=True, text=True,
                                stdin=subprocess.DEVNULL)
        return result.returncode == 0 and json.loads(result.stdout).get('rex_ui') == 1
    except (OSError, ValueError, AttributeError):
        return False


def recorded_state(record):
    """The state the record names, read as the binary reads it
    (locator::read_install_record, and packages.rs's stale rule)."""
    try:
        text = record.read_bytes().decode()
    except (OSError, UnicodeDecodeError):
        return None
    state = Path(text[:-1] if text.endswith('\n') else text)
    if not text or not state.is_absolute():
        return None
    try:
        launcher = os.lstat(state / 'runtime/namedrop')
    except OSError:
        return None
    return state if stat.S_ISREG(launcher.st_mode) and launcher.st_mode & 0o111 else None


def failure(stdout):
    """The message of a failed command's envelope, or its text."""
    try:
        return json.loads(stdout)['error']['message']
    except (ValueError, KeyError, TypeError):
        return stdout.strip()


def show_targets(targets):
    for target in targets:
        print(f"  {target['what']}: {target['path']}")


AGENTS = {'codex': 'Codex', 'claude-code': 'Claude Code'}
started = False
try:
    root, yes = Path(sys.argv[1]), sys.argv[2]
    require(os.environ.get('HOME'), 'Namedrop install needs HOME set to your home directory.')
    home = Path(os.environ['HOME'])
    require(home.is_absolute(), 'Namedrop install needs an absolute HOME.')
    record = home / '.config/namedrop/state-directory'
    state = recorded_state(record)
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
    if not recorded:
        # Herdr releases before step 6 write no record, and a current one
        # whose record was lost has none either. Herdr keeps a plugin's
        # state in $XDG_STATE_HOME/herdr/plugins/<id>, else
        # ~/.local/state/herdr/plugins/<id> (herdr's config::state_dir). That
        # state outlives `herdr plugin uninstall`, so it counts only while
        # Herdr still lists Namedrop.
        base = Path(os.environ.get('XDG_STATE_HOME') or '')
        herdr = (base if base.is_absolute() else home / '.local/state') / 'herdr/plugins/namedrop'
        published = any(os.path.lexists(herdr / 'runtime' / name) for name in ('owner', 'current'))
        if published and herdr_lists_namedrop(home, herdr) and owner(herdr, "Herdr's Namedrop state") == 'herdr':
            # A Herdr release with the Rex module writes the record when it
            # activates; an older one must be updated first.
            if supports_rex(herdr / 'runtime/namedrop'):
                raise ValueError(f'Namedrop is already installed in Herdr ({herdr}). '
                                 'Run Activate Namedrop in Herdr first, then run sh scripts/install.sh again.')
            raise ValueError(f'Namedrop is already installed in Herdr ({herdr}). '
                             'Update Namedrop in Herdr first, then run sh scripts/install.sh again.')
    launcher = state / 'runtime/namedrop'
    decision = 'adopt' if recorded and selected_owner == 'herdr' else 'update' if recorded else 'new'
    if decision == 'adopt':
        # `herdr plugin uninstall` keeps the state and the record that names
        # it, and nothing updates a runtime that Herdr no longer lists.
        require(herdr_lists_namedrop(home, state),
                f"Namedrop's install record names Herdr's Namedrop state ({state}), but Namedrop is no "
                'longer installed in Herdr. Install it in Herdr again, or move that directory away, '
                'then run sh scripts/install.sh again.')
        require(supports_rex(launcher), 'Update Namedrop in Herdr first, then run sh scripts/install.sh again.')
    # The archive's own binary finds the Rex server as `namedrop install`
    # does, and names what that install writes. It writes nothing.
    staged = root / 'bin/namedrop'
    binary = staged if staged.is_file() else root / 'target/aarch64-apple-darwin/release/namedrop'
    require(binary.is_file(), 'No built Namedrop executable was found. Run sh scripts/build.sh, or unpack the release archive again.')
    preview = subprocess.run([str(binary), '--install-preview'], capture_output=True, text=True,
                             stdin=subprocess.DEVNULL)
    if preview.returncode:
        print(preview.stderr.strip() or f'{binary} --install-preview failed with status {preview.returncode}.',
              file=sys.stderr)
        sys.exit(69)
    plan = json.loads(preview.stdout)
    print(f'Namedrop Rex install: {decision}\nState directory: {state}\nLauncher: {launcher}\nRex server: {plan["server"]}')
    print('Files to install or update:')
    if decision != 'adopt':
        print(f'  Runtime: {state}/runtime (releases, current, owner, source.json and migration records) and the store')
    show_targets(plan['targets'])
    if decision == 'adopt':
        print("Herdr keeps its runtime and updates it. This archive publishes no runtime.")
    if yes != 'yes':
        print('Install Namedrop for Rex? [y/N] ', end='', flush=True)
        with os.fdopen(3) as answer:
            confirmed = answer.readline().strip().lower() in ('y', 'yes')
        if not confirmed:
            print('Installation cancelled; no files changed.')
            sys.exit(0)
    started = True
    if decision == 'adopt':
        command = [str(launcher), '--plugin-state', str(state), '--json', 'install',
                   '--input-json', '{"confirmed":true}']
        print('Installing the Rex module and agent packages with Herdr\'s runtime…', flush=True)
    else:
        state.mkdir(parents=True, exist_ok=True)
        command = [sys.executable, str(root / 'scripts/runtime.py'), str(root),
                   '--state-dir', str(state), '--verify', 'install']
        print('Publishing the Namedrop runtime and installing it for Rex…', flush=True)
    # Progress and warnings go straight to the terminal; stdout is the report.
    result = subprocess.run(command, stdout=subprocess.PIPE, text=True, stdin=subprocess.DEVNULL)
    if result.returncode:
        message = failure(result.stdout)
        if message:
            print(message, file=sys.stderr)
        sys.exit(result.returncode)
    report = json.loads(result.stdout)
    require(report.get('status') == 'ok' and report.get('operation') == 'install',
            'Namedrop did not return an installation report. Inspect setup before retrying.')
    data = report['data']
    print(f'Namedrop is installed for Rex.\nModule: {data["module"]}\nConfig: {data["config"]} (init.lua line: {data["line"]})')
    print('Installed or updated:')
    if decision != 'adopt':
        print(f'  Runtime: {state}/runtime and the store')
    show_targets(data.get('targets') or plan['targets'])
    if data.get('manual_config'):
        if data.get('reason'):
            print(data['reason'])
        print(data['instruction'])
    keys = data.get('keys') or []
    if keys:
        print('Keys:')
    for key in keys:
        status = key.get('status')
        effect = ('in effect' if status == 'in_effect'
                  else f"shadowed by {key.get('effective_action')} from {key.get('source')}" if status == 'shadowed'
                  else 'not bound')
        print(f"  {key.get('key')}: {key.get('action')} ({effect})")
    shell = data.get('shell_command')
    if isinstance(shell, dict):
        print(f"Shell command: {shell.get('path')}")
        if shell.get('next_step'):
            print(f"  {shell['next_step']}")
        if shell.get('current_shell'):
            print(f"  current_shell: {shell['current_shell']}")
    guidance = data.get('agent_integration_hook_activation')
    if guidance:
        print('Agent hooks:')
        for agent, value in guidance.items():
            steps = value.get('steps') if isinstance(value, dict) else None
            print(f'  {AGENTS.get(agent, agent)}:')
            for step in steps or []:
                print(f'    {step}')
    print('In the Rex palette, run Activate Namedrop, then Name this pane. Start new agent sessions.')
except KeyboardInterrupt:
    print('\nNamedrop Rex installation interrupted. Run sh scripts/install.sh again to finish it.' if started
          else '\nInstallation cancelled; no files changed.', file=sys.stderr)
    sys.exit(130)
except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
    print(f'Namedrop Rex installation stopped: {error}', file=sys.stderr)
    sys.exit(2)
PY
