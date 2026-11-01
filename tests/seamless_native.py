"""Native Herdr, local Git transport, real Namedrop and a simulated agent.

No model credentials or user profiles are used. The simulator receives the real
delivery envelope; the test executes its bound tool while the same session stays
open across the native managed replacement.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

from support.environment import isolated_home

REPO = Path(__file__).resolve().parent.parent
# A short, canonical root keeps the fixture socket path within Unix limits.
FIXTURE_ROOT = os.path.realpath('/tmp')


def fixture_shell(home):
    """Pane shell environment that marks each prompt as ready.

    NAMEDROP_TEST_SHELL selects bash or zsh; otherwise zsh where installed,
    else bash. Run both where both are installed.
    """
    name = os.environ.get('NAMEDROP_TEST_SHELL') or ('zsh' if shutil.which('zsh') else 'bash')
    path = shutil.which(name)
    assert name in ('bash', 'zsh') and path, f'The fixture shell must be bash or zsh on PATH, not {name!r}'
    ready = ': > "$HOME/shell-ready-$HERDR_PANE_ID"'
    if name == 'zsh':
        (home/'.zshrc').write_text('precmd() { ' + ready + '; }\n')
        return {'SHELL': path, 'ZDOTDIR': str(home)}
    # Login and non-login interactive bash both read .bashrc here.
    (home/'.bashrc').write_text('PROMPT_COMMAND=' + shlex.quote(ready) + '\n')
    (home/'.bash_profile').write_text('. "$HOME/.bashrc"\n')
    return {'SHELL': path}


def compile_agent(path):
    """A native stdin-to-stdout process; Herdr detects it by its file name.

    Compiled rather than copied: copied Apple system executables can be killed
    by code-signing policy, and some Linux coreutils are multi-call binaries.
    """
    subprocess.run(['rustc', '--edition', '2021', '-o', str(path), '-'], check=True, capture_output=True, text=True,
                   input='fn main() { std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap(); }')


def main():
    herdr = shutil.which('herdr')
    assert herdr, 'Native Herdr is required for this acceptance check'
    binary = Path(os.environ.get('NAMEDROP_TEST_BINARY', REPO / 'target/debug/namedrop')).resolve()
    previous = Path(os.environ.get('NAMEDROP_TEST_PREVIOUS_BINARY', binary)).resolve()
    base = Path(tempfile.mkdtemp(prefix='namedrop-native-update-', dir=FIXTURE_ROOT))
    env = {k: v for k, v in os.environ.items() if not k.startswith(('HERDR_', 'CODEX_', 'CLAUDE_', 'NAMEDROP_', 'GIT_CONFIG_'))}
    # Setup runs any codex on PATH, which would install the plugin before the check that it is still missing.
    env['PATH'] = os.pathsep.join(d for d in env['PATH'].split(os.pathsep) if not os.access(os.path.join(d, 'codex'), os.X_OK))
    for name in ('home', 'config', 'state', 'data', 'run', 'workspace', 'remote', 'evidence'):
        (base / name).mkdir()
    env.update(isolated_home(base / 'home'))
    env.update(XDG_CONFIG_HOME=str(base/'config'), XDG_STATE_HOME=str(base/'state'),
               XDG_DATA_HOME=str(base/'data'), XDG_RUNTIME_DIR=str(base/'run'),
               CODEX_HOME=str(base/'home/.codex'), CLAUDE_CONFIG_DIR=str(base/'home/.claude'),
               GIT_CONFIG_GLOBAL=str(base/'home/.gitconfig'), HERDR_SOCKET_PATH=str(base/'run/herdr.sock'),
               **fixture_shell(base/'home'))
    def command(*args, context=None, check=True, timeout=60):
        result = subprocess.run([str(a) for a in args], env=dict(env, **(context or {})),
                                cwd=base/'workspace', capture_output=True, text=True, timeout=timeout)
        with (base/'evidence/commands.jsonl').open('a') as log:
            log.write(json.dumps({'args': [str(a) for a in args], 'exit':result.returncode,
                                  'stdout':result.stdout, 'stderr':result.stderr}) + '\n')
        assert not check or result.returncode == 0, (args, result.stdout, result.stderr)
        return result
    def host(*args, context=None):
        result = command(herdr, *args, context=context)
        return json.loads(result.stdout) if result.stdout.strip() else None
    def wait(predicate, description, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.15)
        raise AssertionError('Timed out: ' + description)
    def git(*args):
        return command('git', '-C', base/'remote', *args).stdout.strip()
    command('git', '-C', base/'workspace', 'init', '--quiet')
    git('init', '--quiet')
    git('config', 'user.name', 'Namedrop acceptance')
    git('config', 'user.email', 'acceptance@example.invalid')
    command('git', 'config', '--global', f'url.{(base/"remote").as_uri()}.insteadOf', 'https://github.com/namedrop-fixture/seamless.git')
    remote = base/'remote'
    (remote/'scripts').mkdir()
    for script in ('setup.sh', 'update.sh', 'update.py', 'run.sh', 'runtime.py'):
        shutil.copyfile(REPO/'scripts'/script, remote/'scripts'/script)
    for file in ('herdr-plugin.toml', 'runtime.json'):
        shutil.copyfile(REPO/file, remote/file)
    # Two valid executables with distinct snapshot identities, built from the
    # same tested Rust code. macOS re-signs each copy; ELF loaders ignore
    # trailing bytes, so Linux appends a revision marker instead.
    digests = {}
    for version in ('one', 'two'):
        destination = base / f'namedrop-{version}'
        shutil.copyfile(previous if version == 'one' else binary, destination)
        destination.chmod(0o700)
        if sys.platform == 'darwin':
            command('codesign', '--force', '--sign', '-', '--identifier', f'namedrop-fixture-{version}', destination)
        else:
            with destination.open('ab') as output:
                output.write(f'\nnamedrop-fixture-revision-{version}\n'.encode())
        assert command(destination, '--version').stdout.startswith('namedrop '), destination
        digests[version] = hashlib.sha256(destination.read_bytes()).hexdigest()
    assert digests['one'] != digests['two']
    (base/'evidence/fixture.json').write_text(json.dumps({
        'platform': platform.platform(), 'shell': env['SHELL'],
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'release_sha256': digests}, indent=2))
    def commit_release(version):
        # The shared source-build contract: scripts/build.sh stages bin/namedrop.
        (remote/'scripts/build.sh').write_text(
            '#!/bin/sh\nset -eu\nmkdir -p bin\n'
            f'cp {shlex.quote(str(base / ("namedrop-" + version)))} bin/namedrop\n'
            'chmod 700 bin/namedrop\n')
        git('add', '.')
        git('commit', '--quiet', '-m', 'Runtime '+version)
        git('branch', '-f', 'guided-update', 'HEAD')
        return git('rev-parse', 'HEAD')
    first = commit_release('one')
    server_log = (base/'evidence/server.log').open('w')
    server = subprocess.Popen([herdr, 'server'], env=env, cwd=base/'workspace',
                              stdout=server_log, stderr=server_log, start_new_session=True)
    successful = False
    try:
        wait(lambda: (base/'run/herdr.sock').exists(), 'disposable server socket')
        created = host('workspace', 'create', '--cwd', str(base/'workspace'), '--label', 'Upgrade acceptance')['result']
        workspace = created['workspace']['workspace_id']
        source = created['root_pane']['pane_id']
        state = base/'state/herdr/plugins/namedrop'
        context = dict(HERDR_ENV='1', HERDR_WORKSPACE_ID=workspace, HERDR_PANE_ID=source,
                       HERDR_PLUGIN_STATE_DIR=str(state))
        command(herdr, 'plugin', 'install', 'namedrop-fixture/seamless', '--ref', 'guided-update', '--yes', context=context)
        registry = host('plugin', 'list', '--plugin', 'namedrop', '--json')['result']['plugins'][0]
        root = Path(registry['plugin_root'])
        package = base/'home/.agents/skills/namedrop'
        (base/'home/.codex').mkdir(exist_ok=True)
        (base/'home/.codex/config.toml').write_text(
            '[marketplaces.namedrop]\nsource_type = "local"\nsource = '+json.dumps(str(package))+
            '\n[plugins."namedrop@namedrop"]\nenabled = true\n')
        command('sh', root/'scripts/setup.sh', 'activate', '--json', context=context)
        launcher = state/'runtime/namedrop'
        assert launcher.is_file()
        assert json.loads((state/'runtime/legacy-sessions.json').read_text())['codex_ready'] is False
        # Model Codex's explicit plugin-add step in this isolated profile, then
        # finish migration before any simulated session can cache instructions.
        version = json.loads((package/'.codex-plugin/plugin.json').read_text())['version']
        cached = base/'home/.codex/plugins/cache/namedrop/namedrop'/version
        cached.parent.mkdir(parents=True)
        shutil.copytree(package, cached)
        (base/'home/.codex/config.toml').write_text(
            '[marketplaces.namedrop]\nsource_type = "local"\nsource = '+json.dumps(str(package))+
            '\n[plugins."namedrop@namedrop"]\nenabled = true\n')
        command('sh', root/'scripts/setup.sh', 'activate', '--json', context=context)
        assert json.loads((state/'runtime/legacy-sessions.json').read_text())['codex_ready'] is True
        def namedrop(*args, pane=source):
            return json.loads(command(launcher, '--herdr-socket', env['HERDR_SOCKET_PATH'], '--herdr-workspace', workspace,
                                      '--herdr-pane', pane, *args, context=context).stdout)
        recipient = host('tab', 'create', '--workspace', workspace, '--label', 'Keep this agent')['result']['root_pane']['pane_id']
        wait(lambda: (base/'home'/f'shell-ready-{recipient}').exists(), 'fixture shell prompt')
        # A native process gives Herdr a real foreground process named codex.
        # It stays alive and records every submitted envelope.
        # No vendor agent or authenticated model is launched.
        simulated_agent = base/'codex'
        compile_agent(simulated_agent)
        def start_agent(pane, session_id, inbox):
            wait(lambda: (base/'home'/f'shell-ready-{pane}').exists(), 'fixture shell prompt')
            simulator = base/f'start-{pane}.sh'
            simulator.write_text("#!/bin/sh\nprintf '\\033]0;Upgrade fixture\\007› \\n'\nstty -icanon -echo min 1 time 0\nexec " +
                                 shlex.quote(str(simulated_agent)) + ' >> ' + shlex.quote(str(inbox)) + '\n')
            host('pane', 'run', pane, shlex.join(['sh', str(simulator)]))
            wait(lambda: any(p['pane_id'] == pane and p.get('agent') == 'codex'
                             and p.get('agent_status') == 'idle' for p in host('pane','list')['result']['panes']),
                 'simulated agent process detection')
            host('pane', 'report-agent-session', pane, '--source', 'herdr:codex', '--agent', 'codex',
                 '--agent-session-id', session_id)
        start_agent(recipient, '11111111-1111-4111-8111-111111111111', base/'envelopes.jsonl')
        start_agent(source, '22222222-2222-4222-8222-222222222222', base/'source-envelopes.jsonl')
        namedrop('name', 'requester', '--type', 'agent')
        namedrop('name', 'reviewer', '--type', 'agent', pane=recipient)
        sessions_before = {p['pane_id']: p['agent_session'] for p in host('pane','list')['result']['panes']
                           if p['pane_id'] in (source, recipient)}
        namedrop('send', '--input-json', json.dumps({'source':'@reviewer keep this handoff open through the update',
                                                    'author_kind':'agent_session', 'request_id':'native-seamless-request'}))
        envelopes = base/'envelopes.jsonl'
        def received_envelope(path=envelopes):
            if path.exists():
                for line in path.read_text().splitlines():
                    try:
                        value = json.loads(line.replace('\x1b[200~','').replace('\x1b[201~','').strip())
                    except ValueError:
                        continue
                    if value.get('control', {}).get('attempt_id'):
                        return value
        envelope = wait(received_envelope, 'real delivery envelope')
        (base/'evidence/envelope.json').write_text(json.dumps(envelope, indent=2))
        assert str(launcher) in json.dumps(envelope), 'The bound tool must use the stable launcher'
        tool_before = (base/'home/.agents/skills/namedrop/SKILL.md').read_bytes()
        layout_before = [(p['pane_id'], p['tab_id'], p['terminal_id']) for p in host('pane','list')['result']['panes']]
        current_before = (state/'runtime/current').resolve()
        assert hashlib.sha256((current_before/'namedrop').read_bytes()).hexdigest() == digests['one']
        with sqlite3.connect(state/'namedrop.sqlite3') as db:
            deliveries_before = db.execute('SELECT delivery_id,state FROM mailbox_deliveries').fetchall()
        assert deliveries_before and any(row[1] not in ('settled','failed','cancelled') for row in deliveries_before)
        second = commit_release('two')
        def open_update(expected='Install update now?'):
            existing = {p['pane_id'] for p in host('pane','list')['result']['panes']}
            host('plugin','action','invoke','update','--plugin','namedrop', context=context)
            pane = wait(lambda: next((p['pane_id'] for p in host('pane','list')['result']['panes']
                                     if p['pane_id'] not in existing), None), 'update tab')
            def output():
                return command(herdr,'pane','read',pane,'--source','recent-unwrapped','--lines','100').stdout
            wait(lambda: expected in output(), 'live update result', timeout=60)
            if expected == 'Install update now?':
                assert 'sessions can keep working' in output(), output()
            return pane, output
        update_pane, output = open_update()
        host('pane','run',update_pane,'y')
        wait(lambda: '[2/3] Install' in output(), 'installation started')
        assert namedrop('delivery', 'inbox', '--input-json', '{}').get('status') == 'ok'
        wait(lambda: 'Update complete.' in output(), 'native live update completion', timeout=60)
        (base/'evidence/update.txt').write_text(output())
        after = host('plugin','list','--plugin','namedrop','--json')['result']['plugins'][0]
        assert registry['source']['resolved_commit'] == first and after['source']['resolved_commit'] == second
        for field in ('kind','owner','repo','requested_ref','managed_path','subdir'):
            assert registry['source'].get(field) == after['source'].get(field), field
        assert (state/'runtime/current').resolve() != current_before and current_before.is_dir()
        assert hashlib.sha256((state/'runtime/current/namedrop').read_bytes()).hexdigest() == digests['two']
        live = host('pane','list')['result']['panes']
        assert {p['pane_id']: p['agent_session'] for p in live if p['pane_id'] in sessions_before} == sessions_before
        assert set(layout_before).issubset({(p['pane_id'],p['tab_id'],p['terminal_id']) for p in live})
        assert (base/'home/.agents/skills/namedrop/SKILL.md').read_bytes() == tool_before
        # Cached instructions still invoke the exact same executable and native
        # session. Finish the request after the checkout has been replaced.
        tool = envelope['control']['tool']
        (base/'evidence/tool.json').write_text(json.dumps(tool, indent=2))
        bound = next(operation for operation in tool['operations'] if operation['token'] == 'reply')
        reply_input = dict(bound['input'])
        reply_input['source'] = reply_input['source'].replace('<reply body>', 'completed after update')
        reply = namedrop(*bound['argv'],'--input-json', json.dumps(reply_input), pane=recipient)
        assert reply.get('status') == 'ok', reply
        with sqlite3.connect(state/'namedrop.sqlite3') as db:
            assert db.execute('SELECT count(*) FROM delivery_dispositions WHERE attempt_id=?', (envelope['control']['attempt_id'],)).fetchone()[0] == 1
        returned = wait(lambda: received_envelope(base/'source-envelopes.jsonl'), 'reply at original source session')
        assert 'completed after update' in json.dumps(returned)
        completion = next(operation for operation in returned['control']['tool']['operations']
                          if operation['token'] == 'complete.no-reply')
        assert namedrop(*completion['argv'], '--input-json', json.dumps(completion['input'])).get('status') == 'ok'
        # A moving branch is rejected by Git's checkout guard before replacement.
        (remote/'note.txt').write_text('inspected target')
        git('add','.'); git('commit','--quiet','-m','Inspected target'); git('branch','-f','guided-update','HEAD')
        race_pane, race_output = open_update()
        (remote/'note.txt').write_text('branch moved')
        git('add','.'); git('commit','--quiet','-m','Moved target'); git('branch','-f','guided-update','HEAD')
        host('pane','run',race_pane,'y')
        wait(lambda: 'Update stopped' in race_output(), 'changed target refusal', timeout=60)
        assert host('plugin','list','--plugin','namedrop','--json')['result']['plugins'][0]['source']['resolved_commit'] == second
        assert 'source changed after inspection' in (state/'last-update.log').read_text()
        assert {p['pane_id']: p['agent_session'] for p in host('pane','list')['result']['panes']
                if p['pane_id'] in sessions_before} == sessions_before
        assert namedrop('delivery', 'inbox', '--input-json', '{}', pane=recipient).get('status') == 'ok'
        git('tag', '-a', 'annotated-release', '-m', 'Annotated release fixture')
        command(herdr, 'plugin', 'install', 'namedrop-fixture/seamless', '--ref', 'annotated-release', '--yes', context=context)
        _, pinned_output = open_update('Already up to date.')
        assert 'Update stopped' not in pinned_output()
        successful = True
        print('native seamless upgrade checks passed')
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill(); server.wait(timeout=5)
        server_log.close()
        if successful:
            evidence = REPO/'.scratch/guided-update/seamless-native'/base.name
            evidence.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(base/'evidence', evidence)
            shutil.rmtree(base)
            print('Evidence:', evidence)
        else:
            print('Native fixture evidence:', base/'evidence', file=sys.stderr)


if __name__ == '__main__':
    main()
