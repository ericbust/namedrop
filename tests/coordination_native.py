"""Candidate coordination through real Herdr events, without a model or credentials.

All installs, sessions, commands and cleanup belong to one disposable profile.
A compiled cat receives envelopes and a renderer drives native terminal detection;
this proves transport/actions, not model
reasoning or presentation. Failed runs retain their evidence and fixture root.
"""
import argparse
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

from seamless_native import FIXTURE_ROOT, compile_agent, fixture_shell

REPO = Path(__file__).resolve().parent.parent


RENDERER = '''import pathlib, sys, time
surface = pathlib.Path(sys.argv[1])
previous = None
while True:
    state = surface.read_text()
    if state != previous:
        title = ('⠋ ' if state == 'working' else '') + 'Synthetic coordination'
        body = '• Working (esc to interrupt)' if state == 'working' else '› '
        sys.stdout.write('\\x1b]0;' + title + '\\x07\\x1b[2J\\x1b[H' + body + '\\n')
        sys.stdout.flush()
        previous = state
    time.sleep(.05)
'''


def isolated_env(base):
    env = {k: os.environ[k] for k in ('PATH', 'LANG', 'LC_ALL', 'TERM', 'TMPDIR') if k in os.environ}
    env.update(HOME=str(base/'home'), XDG_CONFIG_HOME=str(base/'config'), XDG_STATE_HOME=str(base/'state'),
               XDG_DATA_HOME=str(base/'data'), XDG_RUNTIME_DIR=str(base/'run'),
               CODEX_HOME=str(base/'home/.codex'), CLAUDE_CONFIG_DIR=str(base/'home/.claude'),
               GIT_CONFIG_GLOBAL=str(base/'home/.gitconfig'), GIT_CONFIG_NOSYSTEM='1',
               HERDR_SOCKET_PATH=str(base/'run/herdr.sock'), **fixture_shell(base/'home'))
    return env


def read_envelopes(path):
    values = []
    if path.exists():
        for line in path.read_text().splitlines():
            try:
                value = json.loads(line.replace('\x1b[200~', '').replace('\x1b[201~', '').strip())
            except ValueError:
                continue
            if isinstance(value, dict) and value.get('control', {}).get('attempt_id'):
                values.append(value)
    return values


def cleanup_status(pids, sockets, missing_handles=()):
    live = {}
    for name, pid in pids.items():
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        except PermissionError:
            pass  # An inaccessible process is not verified gone.
        live[name] = pid
    remaining = [str(path) for path in sockets if path.exists()]
    return {'verified': not (live or remaining or missing_handles), 'live_pids': live,
            'remaining_sockets': remaining, 'missing_handles': list(missing_handles)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), binary
    herdr = shutil.which('herdr')
    assert herdr, 'Native Herdr with Codex terminal detection is required'
    base = Path(tempfile.mkdtemp(prefix='namedrop-coordination-', dir=FIXTURE_ROOT))
    for name in ('home', 'config', 'state', 'data', 'run', 'workspace', 'plugin', 'evidence'):
        (base / name).mkdir()
    env = isolated_env(base)
    commands = []

    def command(*argv, context=None, timeout=60):
        result = subprocess.run([str(a) for a in argv], env=dict(env, **(context or {})),
                                cwd=base/'workspace', capture_output=True, text=True, timeout=timeout)
        row = {'time_ns': time.time_ns(), 'args': [str(a) for a in argv], 'exit': result.returncode,
               'stdout': result.stdout, 'stderr': result.stderr}
        commands.append(row)
        with (base/'evidence/commands.jsonl').open('a') as log:
            log.write(json.dumps(row) + '\n')
        assert result.returncode == 0, row
        return result.stdout

    def host(*argv, context=None):
        output = command(herdr, *argv, context=context)
        return json.loads(output) if output.strip() else None

    def wait(predicate, description, timeout=35):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = predicate()
            if result:
                return result
            time.sleep(.15)
        raise AssertionError('Timed out: ' + description)

    def save(name, value):
        (base/'evidence'/name).write_text(json.dumps(value, indent=2) + '\n')

    command('git', '-C', base/'workspace', 'init', '--quiet')
    version = command(herdr, '--version').strip()
    capability = command(herdr, 'pane', 'report-agent-session', '--help')
    assert '--agent-session-id' in capability, 'Host lacks native session reporting'
    root = base/'plugin'
    (root/'scripts').mkdir()
    (root/'bin').mkdir()
    for name in ('setup.sh', 'run.sh', 'runtime.py', 'update.py'):
        shutil.copyfile(REPO/'scripts'/name, root/'scripts'/name)
    for name in ('herdr-plugin.toml', 'runtime.json'):
        shutil.copyfile(REPO/name, root/name)
    shutil.copyfile(binary, root/'bin/namedrop')
    (root/'bin/namedrop').chmod(0o700)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    save('candidate.json', {'binary': str(binary), 'sha256': digest, 'host': version,
                            'platform': platform.platform(), 'shell': env['SHELL'], 'fixture': str(base),
                            'simulation': 'compiled cat plus OSC title renderer; no model'})
    # A live owned child must prevent cleanup success; its reaped counterpart
    # must pass the same predicate used for the native fixture below.
    probe = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                             env=env, cwd=base, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        negative = cleanup_status({'probe': probe.pid}, [])
        assert not negative['verified'] and negative['live_pids'] == {'probe': probe.pid}
    finally:
        probe.terminate()
        probe.wait(timeout=5)
    positive = cleanup_status({'probe': probe.pid}, [])
    assert positive['verified'], positive
    save('cleanup-control.json', {'live_child': negative, 'reaped_child': positive})
    server_log = (base/'evidence/server.log').open('w')
    server = subprocess.Popen([herdr, 'server'], env=env, cwd=base/'workspace',
                              stdout=server_log, stderr=server_log, start_new_session=True)
    scenario_success = False
    pid_files = {}
    try:
        wait(lambda: (base/'run/herdr.sock').exists(), 'disposable server socket')
        created = host('workspace', 'create', '--cwd', base/'workspace', '--label', 'Synthetic coordination')['result']
        workspace = created['workspace']['workspace_id']
        panes = {'lead': created['root_pane']['pane_id']}
        for name in ('worker', 'reviewer'):
            panes[name] = host('tab', 'create', '--workspace', workspace, '--label', 'Synthetic '+name)['result']['root_pane']['pane_id']
        state = base/'state/herdr/plugins/namedrop'
        context = dict(HERDR_ENV='1', HERDR_WORKSPACE_ID=workspace, HERDR_PANE_ID=panes['lead'], HERDR_PLUGIN_STATE_DIR=str(state))
        host('plugin', 'link', root, context=context)
        command('sh', root/'scripts/setup.sh', 'activate', '--json', context=context)
        launcher = state/'runtime/namedrop'
        assert launcher.is_file()
        assert hashlib.sha256((state/'runtime/current/namedrop').read_bytes()).hexdigest() == digest
        simulator = base/'codex'
        compile_agent(simulator)
        sessions = {name: f'{i:08d}-1111-4111-8111-{i:012d}' for i, name in enumerate(panes, 1)}
        paths = {name: base/f'{name}-envelopes.jsonl' for name in panes}
        surfaces = {name: base/f'{name}-surface' for name in panes}
        for surface in surfaces.values():
            surface.write_text('idle')
        renderer = base/'render.py'
        renderer.write_text(RENDERER)

        def live():
            return host('pane', 'list')['result']['panes']

        # Native Codex reports identify the session; the terminal detector owns
        # status. Only render fixture output, then require the host to observe it.
        def report(name, status):
            previous = next(p for p in live() if p['pane_id'] == panes[name])['agent_status']
            started = time.time_ns()
            staged = surfaces[name].with_suffix('.next')
            staged.write_text(status)
            staged.replace(surfaces[name])
            observed = wait(lambda: next((p for p in live() if p['pane_id'] == panes[name]
                                          and p.get('agent_status') == status
                                          and (p.get('agent_session') or {}).get('value') == sessions[name]), None),
                            f'{name} detected {status} with original session')
            if previous != status:
                def hook_finished():
                    logs = host('plugin', 'log', 'list', '--plugin', 'namedrop', '--limit', '100')['result']['logs']
                    return any(row.get('event') == 'pane.agent_status_changed' and row.get('exit_code') == 0
                               and row['started_unix_ms'] >= started // 1_000_000 for row in logs)
                wait(hook_finished, 'real status-change hook completed')
                wait(lambda: not list((state/'agent-status-reconciliation').glob('*/pending-*')),
                     'native status reconciliation processed')
            with (base/'evidence/statuses.jsonl').open('a') as log:
                log.write(json.dumps({'requested_ns': started, 'observed_ns': time.time_ns(),
                                      'surface': status, 'previous': previous, 'pane': observed}) + '\n')

        def namedrop(*argv, name='lead'):
            response = json.loads(command(launcher, '--plugin-state', state, *argv,
                                          context=dict(context, HERDR_PANE_ID=panes[name])))
            assert response.get('status') == 'ok', response
            return response['data']

        def action(envelope, token, *, outcome=None, assignment=None):
            return next(a for a in envelope['control']['tool']['operations'] if a['token'] == token
                        and (outcome is None or a['input'].get('outcome') == outcome)
                        and (assignment is None or a['input'].get('assignment_id') == assignment))

        def bound(envelope, operation, **authored):
            tool = envelope['control']['tool']
            assert Path(tool['executable']) == launcher
            bound_env = tool['environment']
            assert bound_env['HERDR_SOCKET_PATH'] == env['HERDR_SOCKET_PATH']
            assert bound_env['HERDR_WORKSPACE_ID'] == workspace
            assert bound_env['HERDR_PANE_ID'] in panes.values()
            assert bound_env['HERDR_PLUGIN_STATE_DIR'] == str(state)
            body = dict(operation['input'], **authored)
            # These identities may never be replaced by authored content.
            for field in ('attempt_id', 'in_reply_to', 'coordination_id', 'assignment_id', 'expected_revision', 'request_id'):
                if field in operation['input']:
                    assert body[field] == operation['input'][field]
            argv = [tool['executable'], '--plugin-state', bound_env['HERDR_PLUGIN_STATE_DIR'],
                    *operation['argv'], tool['input_flag'], json.dumps(body),
                    '--herdr-socket', bound_env['HERDR_SOCKET_PATH'], '--herdr-workspace', bound_env['HERDR_WORKSPACE_ID'],
                    '--herdr-pane', bound_env['HERDR_PANE_ID']]
            result = json.loads(command(*argv, context=bound_env))
            assert result.get('status') == 'ok', result
            return result['data']

        def rows(sql, parameters=()):
            with sqlite3.connect((state/'namedrop.sqlite3').as_uri()+'?mode=ro', uri=True) as db:
                return db.execute(sql, parameters).fetchall()

        def count_wakes():
            return rows("SELECT count(*) FROM collaboration_messages WHERE grammar_version='coordination-nudge/v1'")[0][0]

        def received(name, message):
            return wait(lambda: next((e for e in read_envelopes(paths[name]) if e['control']['message_id'] == message), None),
                        f'{name} message {message}')

        def settle_return(message):
            report('lead', 'idle')
            receipt = received('lead', message)
            report('lead', 'working')
            bound(receipt, action(receipt, 'complete.no-reply'))
            return receipt

        for name, pane in panes.items():
            wait(lambda: (base/'home'/f'shell-ready-{pane}').exists(), 'fixture shell prompt')
            script = base/f'start-{name}.sh'
            pid_files[name] = base/f'{name}-pids'
            script.write_text('#!/bin/sh\nstty -icanon -echo min 1 time 0\n'
                              + shlex.join([sys.executable, str(renderer), str(surfaces[name])]) + ' &\n'
                              + 'printf \'%s\\n\' "$$" "$!" > ' + shlex.quote(str(pid_files[name])) + '\nexec '
                              + shlex.quote(str(simulator)) + ' >> ' + shlex.quote(str(paths[name])) + '\n')
            host('pane', 'run', pane, shlex.join(['sh', str(script)]))
            wait(lambda: pid_files[name].exists() and len(pid_files[name].read_text().splitlines()) == 2,
                 'owned simulator and renderer PID handles')
            wait(lambda: any(p['pane_id'] == pane and p.get('agent') == 'codex' for p in live()), 'simulator detection')
            host('pane', 'report-agent-session', pane, '--source', 'herdr:codex', '--agent', 'codex',
                 '--agent-session-id', sessions[name])
            report(name, 'working' if name == 'lead' else 'idle')
            namedrop('name', name, '--type', 'agent', name=name)
        save('identities.json', {'workspace': workspace, 'panes': panes, 'sessions': sessions,
                                 'registrations': rows('SELECT address_id,agent_session_id,capability_profile FROM integration_registrations')})
        started = namedrop('handoff', 'begin', '--input-json', json.dumps({'request_id': 'native-begin', 'deliverable': 'Review checksum and bounds, then report one synthetic answer.'}))
        coordination = started['coordination_id']
        requests = {}
        for name, body in (('worker', 'Compute the synthetic checksum.'), ('reviewer', 'Independently review synthetic bounds.')):
            requests[name] = namedrop('send', '--input-json', json.dumps({'request_id': 'native-'+name, 'author_kind': 'agent_session',
                                                'source': '@'+name+' '+body, 'coordination_id': coordination}))
        assert requests['worker']['message_id'] != requests['reviewer']['message_id']
        returns = []
        for name in ('worker', 'reviewer'):
            e = received(name, requests[name]['message_id'])
            returns.append(bound(e, action(e, 'reply'), source='@lead '+('Checksum progress; final edge remains.' if name == 'worker' else 'Bounds including zero checked.'),
                                 work_outcome='needs_continuation' if name == 'worker' else 'completed'))
        for receipt in returns:
            settle_return(receipt['message_id'])
        assert count_wakes() == 0, 'Busy owner must not receive a nudge'
        # Native pause is a negative control. Resume while working so it cannot
        # be the decisive wake; only the subsequent observed idle event can be.
        namedrop('space', 'pause')
        report('lead', 'idle')
        assert rows('SELECT paused FROM worktrees WHERE worktree_id=?', (workspace,)) == [(1,)]
        assert count_wakes() == 0
        report('lead', 'working')
        namedrop('space', 'resume')
        assert rows('SELECT paused FROM worktrees WHERE worktree_id=?', (workspace,)) == [(0,)]
        assert count_wakes() == 0
        before = host('plugin', 'log', 'list', '--plugin', 'namedrop', '--limit', '100')
        save('hooks-before.json', before)
        event_start = len(commands)
        report('lead', 'idle')
        wake = wait(lambda: next((e for e in read_envelopes(paths['lead']) if e['control'].get('coordination_nudge')), None), 'native idle-event continuation')
        after = host('plugin', 'log', 'list', '--plugin', 'namedrop', '--limit', '100')
        save('hooks-after.json', after)
        decisive = commands[event_start:]
        assert all(row['args'][0] == herdr for row in decisive), 'No manual Namedrop operation may drive the decisive wake'
        before_logs = {row['log_id'] for row in before['result']['logs']}
        new_logs = [row for row in after['result']['logs'] if row['log_id'] not in before_logs]
        assert any(row.get('event') == 'pane.agent_status_changed' and row.get('exit_code') == 0
                   and 'agent-status-changed' in row['command'] for row in new_logs), 'Missing new successful plugin status-hook log'
        assert count_wakes() == 1
        save('decisive-event.json', {'commands': decisive, 'wake': wake})
        for _ in range(2):
            report('lead', 'working')
            report('lead', 'idle')
        assert count_wakes() == 1, 'Same semantic revision must not create another wake'
        report('lead', 'working')
        current = bound(wake, action(wake, 'handoff.results'))
        assert current['outcome'] == 'open' and len(current['assignments']) == 2
        assignment = requests['worker']['coordination'][0]['assignment_id']
        corrected = bound(wake, action(wake, 'send', assignment=assignment), source='@worker Correct the last checksum edge and finish.')
        assert corrected['coordination'][0]['revision'] == 2
        bound(wake, action(wake, 'complete.no-reply'))
        request = received('worker', corrected['message_id'])
        result = bound(request, action(request, 'reply'), source='@lead Checksum 42; last edge corrected and zero verified.', work_outcome='completed')
        settle_return(result['message_id'])
        report('lead', 'idle')
        final_wake = wait(lambda: next((e for e in read_envelopes(paths['lead']) if e['control'].get('coordination_nudge')
                                        and e['control']['message_id'] != wake['control']['message_id']), None), 'final review continuation')
        report('lead', 'working')
        current = bound(final_wake, action(final_wake, 'handoff.results'))
        assert all(a['work_outcome'] == 'completed' for a in current['assignments'])
        saved = bound(final_wake, action(final_wake, 'handoff.resolve', outcome='completed'), report='Synthetic checksum 42 and bounds reviewed; last edge and zero verified.')
        bound(final_wake, action(final_wake, 'complete.no-reply'))
        report('lead', 'idle')
        final = received('lead', saved['report_message_id'])
        assert final['control']['reply_mode'] == 'report_to_human' and final['untrusted_body'].strip()
        bound(final, action(final, 'complete.no-reply'))
        persisted = namedrop('handoff', 'results', '--input-json', json.dumps({'coordination_id': coordination}))
        assert persisted['report'] == final['untrusted_body'] and persisted['report_saved'] is True
        assert persisted['report_delivery_settled'] is True and persisted['presentation'] == 'unknown'
        assert count_wakes() == 2
        # A completed coordination whose report is saved and settled has nothing
        # left to act on, so its warning is resolved (presentation stays unknown).
        assert rows("SELECT count(*) FROM attention_items WHERE source_kind='coordination' AND resolved_at_ms IS NULL")[0][0] == 0
        assert rows("SELECT count(*) FROM attention_items WHERE source_kind='coordination'")[0][0] > 0
        for name in panes:
            save(name+'-envelopes.json', read_envelopes(paths[name]))
        attempts = rows('SELECT attempt_id,address_id,agent_session_id,delivery_id FROM delivery_attempts')
        dispositions = rows('SELECT attempt_id,disposition_kind FROM delivery_dispositions')
        for e in read_envelopes(paths['lead']):
            attempt = e['control']['attempt_id']
            assert len([d for d in dispositions if d[0] == attempt]) == 1
            assert next(a[2] for a in attempts if a[0] == attempt) == sessions['lead']
        save('result.json', {'coordination': persisted, 'attempts': attempts, 'dispositions': dispositions,
                             'native_event': True, 'actual_model': False, 'presentation': 'unknown'})
        with sqlite3.connect(state/'namedrop.sqlite3') as source, sqlite3.connect(base/'evidence/state.sqlite3') as destination:
            source.backup(destination)
        scenario_success = True
    finally:
        if server.poll() is None:
            server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)
        server_log.close()
        owned = {'server': server.pid}
        missing = []
        for name, path in pid_files.items():
            try:
                cat, renderer_pid = map(int, path.read_text().splitlines())
                assert cat > 1 and renderer_pid > 1
                owned.update({name+'-cat': cat, name+'-renderer': renderer_pid})
            except (OSError, ValueError, AssertionError):
                missing.append(str(path))
        sockets = [base/'run/herdr.sock', base/'run/herdr-client.sock']
        deadline = time.monotonic() + 10
        cleanup = cleanup_status(owned, sockets, missing)
        while not cleanup['verified'] and time.monotonic() < deadline:
            time.sleep(.15)
            cleanup = cleanup_status(owned, sockets, missing)
        save('cleanup.json', {'server_exit': server.returncode, 'owned_pids': owned,
                              'scope': str(base), 'scenario_success': scenario_success, **cleanup})
        if scenario_success and cleanup['verified']:
            evidence = REPO/'.scratch/leader-continuation/native'/base.name
            evidence.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(base/'evidence', evidence)
            shutil.rmtree(base)
            print('Evidence:', evidence)
            print('NAMEDROP_COORDINATION_NATIVE_OK')
        else:
            print('Failed native fixture retained:', base, file=sys.stderr)
            if scenario_success:
                raise AssertionError(f'Native scenario passed but cleanup is incomplete: {cleanup}')


if __name__ == '__main__':
    main()
