"""Isolated real-process/PTY checks; the host is fake, activation uses setup.sh."""
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
from runtime import LAUNCHER
SCHEMA_VERSION = int(re.search(r'const SCHEMA_VERSION: u32 = (\d+);', (REPO/'src/store.rs').read_text())[1])
assert int(re.search(r'^SCHEMA_VERSION = (\d+)$', (REPO/'scripts/update.py').read_text(), re.M)[1]) == SCHEMA_VERSION, 'Updater preflight must be reviewed when the store schema changes'
HOST = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sqlite3, sys
base = pathlib.Path(os.environ['FIXTURE'])
args = sys.argv[1:]
with (base/'calls').open('a') as f:
    f.write(json.dumps({'args': args, 'cwd': os.getcwd()})+'\n')
mode = os.environ['CASE']
def emit(value):
    print(json.dumps(value))
if args[:2] == ['plugin', 'list']:
    value = json.loads((base/'registry').read_text())
    if mode == 'malformed': value = {'result': {'plugins': {}}}
    emit(value)
elif args[:2] == ['session', 'list']:
    other = mode in ('other-server', 'other-server-idle', 'other-server-unavailable', 'wait-other-server')
    emit({'sessions': [{'running': True, 'socket_path': 'socket'}] +
          ([{'running': True, 'socket_path': '/another/profile.sock'}] if other else [])})
elif args[:2] == ['pane', 'list']:
    is_other = os.environ['HERDR_SOCKET_PATH'] != 'socket'
    if is_other and mode == 'other-server-unavailable': sys.exit('other server unavailable')
    count = base/('other-pane-count' if is_other else 'pane-count')
    n = int(count.read_text())+1 if count.exists() else 1
    # Replace atomically: the harness reads pane-count while this runs.
    tmp = count.with_name(f'{count.name}.{os.getpid()}')
    tmp.write_text(str(n))
    os.replace(tmp, count)
    panes = [{'pane_id': 'w:p', 'workspace_id': 'w'},
             dict(pane_id='other:p', workspace_id='other', tab_id='other:t', terminal_id='term1', title='@main'),
             dict(pane_id='third:p', workspace_id='third', tab_id='third:t', terminal_id='term2', label='Reviewer')]
    agents = mode in ('busy', 'label-fallback', 'wait-finishes', 'wait-registry-change', 'wait-broken-query', 'wait-pause-change', 'wait-chatter')
    agents = agents or mode in ('busy-after-confirm', 'wait-reconfirm') and n > 1
    agents = agents or is_other and mode in ('other-server', 'wait-other-server')
    if agents and not (base/'finished').exists():
        panes[1].update(agent='codex', agent_session={'value':'session1'}, agent_status='idle', revision=n)
        panes[2].update(agent_session={'agent':'claude', 'value':'session2'}, agent_status='working', revision=n)
    if mode in ('receipt-live', 'join-eligible') or mode in ('receipt-returned', 'join-returned') and n > 1:
        panes[1].update(agent='codex', agent_session={'value':'source-session'})
    if mode in ('receipt-unknown', 'join-unknown'):
        panes[1].update(agent='codex')
    if mode == 'wait-broken-query' and (base/'finished').exists(): panes = None
    # Ending a session leaves the SAME panes/tabs/workspaces in place.
    emit({'result': {'panes': panes}})
elif args[:2] == ['workspace', 'list']:
    if mode == 'label-fallback': sys.exit('labels unavailable')
    emit({'result': {'workspaces': [{'workspace_id':'other', 'label':'Research'},
                                    {'workspace_id':'third', 'label':'Namedrop'}]}})
elif args[:2] == ['tab', 'list']:
    emit({'result': {'tabs': [{'tab_id':'other:t', 'label':'@main', 'number':2},
                            {'tab_id':'third:t', 'label':'Review', 'number':1}]}})
elif args[:2] == ['plugin', 'install']:
    (base/'installed').touch()
    assert not pathlib.Path.cwd().is_relative_to(base/'old root')
    if mode == 'install-fail': sys.exit(7)
    reg = json.loads((base/'registry').read_text())
    relative = pathlib.Path(reg['result']['plugins'][0]['plugin_root']).relative_to(base/'old root')
    shutil.rmtree(base/'old root')
    reg['result']['plugins'][0]['plugin_root'] = str(base/'new root'/relative)
    reg['result']['plugins'][0]['source']['managed_path'] = str(base/'new root')
    pinned = reg['result']['plugins'][0]['source']['requested_ref'].lower() == 'a'*40
    reg['result']['plugins'][0]['source']['resolved_commit'] = ('a' if mode == 'unchanged' or pinned else 'b')*40
    (base/'registry').write_text(json.dumps(reg))
    emit({'result': {'type': 'plugin_installed'}})
elif args[:2] == ['integration', 'status']:
    emit({'result': {}})
elif args[:3] == ['plugin', 'action', 'invoke']:
    emit({'result': {'type': 'plugin_action_invoked', 'log': {'log_id': 'activation'}}})
elif args[:3] == ['plugin', 'log', 'list']:
    with sqlite3.connect(pathlib.Path(os.environ['HERDR_PLUGIN_STATE_DIR'])/'namedrop.sqlite3') as db:
        if mode.startswith('schema-bump'):
            db.execute('PRAGMA user_version=@@SCHEMA_BUMP@@')
            db.execute('DROP TABLE IF EXISTS mailbox_deliveries')
        if mode in ('pause-change', 'schema-bump-pause-change', 'human-resume', 'plugin-resume'):
            db.execute('UPDATE worktrees SET paused=0')
        if mode in ('human-resume', 'plugin-resume'):
            db.execute("INSERT INTO transition_facts VALUES ('socket','w',1,'worktree','w','resumed',?,'fixture',1)",
                       ('human' if mode == 'human-resume' else 'plugin',))
        if mode == 'schema-bump-layout':
            db.execute('ALTER TABLE worktrees RENAME TO migrated_worktrees')
    failure = mode == 'activation-fail'
    payload = {'data': {'space_id':'w', 'state_directory': os.environ['HERDR_PLUGIN_STATE_DIR'],
                       'integration_packages':{'codex': {'installed': True, 'path':'fixture'}}}}
    if mode == 'live-drain-error':
        payload['data'].update(runtime={'live_update':True}, drain={'error':'fixture reconciliation failure'},
            agent_integration_hook_activation={'codex':{'steps':['codex plugin add namedrop@namedrop --json']}})
    emit({'result': {'type':'plugin_log_list', 'logs': [{'log_id':'activation', 'plugin_id':'namedrop',
        'action_id':'activate', 'status': 'failed' if failure else 'succeeded', 'exit_code':5 if failure else 0,
        'stdout':json.dumps(payload), 'stderr':'activation error' if failure else ''}]}})
elif args[:3] == ['plugin', 'pane', 'open']:
    assert '--target-pane' not in args and '--placement' in args and 'tab' in args
    emit({'result': {'type': 'plugin_pane_opened'}})
else:
    sys.exit('unexpected: '+repr(args))
'''

GIT = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
if args[:1] == ['-C']: args = args[2:]
if args[:1] in (['init'], ['fetch']): pass
elif args[:1] == ['rev-parse']:
    registry = json.loads((pathlib.Path(os.environ['FIXTURE'])/'registry').read_text())
    pinned = registry['result']['plugins'][0]['source']['requested_ref'].lower() == 'a'*40
    print(('a' if os.environ['CASE'] == 'unchanged' or pinned else 'b') * 40)
elif args[:1] == ['show']:
    if os.environ.get('LIVE') != '1': sys.exit(1)
    path = args[1].split(':', 1)[1]
    repo = pathlib.Path(os.environ['TEST_REPO'])
    if path.endswith('runtime.json'):
        value = json.loads((repo/'runtime.json').read_text())
        if os.environ.get('INCOMPATIBLE'): value['live_upgrade_epoch'] += 1
        print(json.dumps(value))
    elif path.endswith('herdr-plugin.toml'): print((repo/'herdr-plugin.toml').read_text())
    elif path.endswith('scripts/run.sh'): print((repo/'scripts/run.sh').read_text())
    else: sys.exit('unexpected show')
else: sys.exit('unexpected git: '+repr(args))
'''


def case(mode, answer=b'yes\n', ref='main', subdir=None, interactive=True, choice=b'2\n', waiting=b'\n', second_answer=b'yes\n', live=False, legacy=False, incompatible=False, codex_ready=True, prior_schema=None, built=None):
    with tempfile.TemporaryDirectory(prefix='namedrop-update-test-') as directory:
        base = Path(directory)
        shutil.copyfile(REPO/'tests/setup_socket.py', base/'socket.py')
        old = base/'old root'/(subdir or '')
        new = base/'new root'/(subdir or '')
        for root in (old, new):
            (root/'scripts').mkdir(parents=True)
            for name in ('update.sh', 'update.py', 'setup.sh', 'run.sh', 'runtime.py'):
                shutil.copyfile(REPO/'scripts'/name, root/'scripts'/name)
        (base/'bin').mkdir()
        host = base/'bin/herdr'
        host.write_text(HOST.replace('@@SCHEMA_BUMP@@', str(SCHEMA_VERSION + 1)))
        host.chmod(0o700)
        git = base/'bin/git'
        git.write_text(GIT)
        git.chmod(0o700)
        state = base/'state'
        state.mkdir()
        if live:
            runtime = state / 'runtime'
            snapshot = runtime / 'releases' / ('a' * 64)
            snapshot.mkdir(parents=True)
            shutil.copyfile(REPO/'runtime.json', snapshot/'runtime.json')
            if prior_schema is not None:
                prior = json.loads((snapshot/'runtime.json').read_text())
                prior['state_schema'] = prior_schema
                prior.pop('live_migrate_from', None)
                prior.pop('command_gate', None)
                (snapshot/'runtime.json').write_text(json.dumps(prior))
            executable = snapshot/'namedrop'
            executable.write_text('#!/bin/sh\nexit 0\n')
            executable.chmod(0o700)
            (runtime/'current').symlink_to(Path('releases')/('a'*64))
            (runtime/'namedrop').write_text(LAUNCHER)
            (runtime/'namedrop').chmod(0o700)
            (runtime/'legacy-sessions.json').write_text(json.dumps({'sessions':[['socket','session1']] if legacy else [], 'codex_ready':codex_ready, 'packages_ready':True}))
            if built:
                # The installed checkout's executable, compared with the active
                # runtime: staged bin/namedrop first, else the older Mac output.
                (runtime/'activation.json').write_text(json.dumps({'revision': 'a'*64}))
                for relative, contents in {'bin': [('bin/namedrop', executable.read_bytes())],
                                           'target': [('target/aarch64-apple-darwin/release/namedrop', executable.read_bytes())],
                                           'stale-bin': [('bin/namedrop', b'#!/bin/sh\nexit 1\n'),
                                                         ('target/aarch64-apple-darwin/release/namedrop', executable.read_bytes())]}[built]:
                    (old/relative).parent.mkdir(parents=True, exist_ok=True)
                    (old/relative).write_bytes(contents)
        with sqlite3.connect(state/'namedrop.sqlite3') as db:
            schema = (REPO/'src/store.rs').read_text().split('const SCHEMA: &str = r#"', 1)[1].split('"#;', 1)[0]
            db.executescript(schema)
            db.executescript((REPO/'src/store/coordination.rs').read_text().split('const SCHEMA: &str = r#"', 1)[1].split('"#;', 1)[0])
            db.execute(f'PRAGMA user_version={SCHEMA_VERSION}')
            db.execute("INSERT INTO worktrees VALUES ('socket', 'w', 1, 1, 0, 0)")
            if mode == 'uncertain':
                db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w','d','a',1,'m',?,0)",
                           ('settled' if mode == 'uncertain' else 'held',))
            if mode == 'schema': db.execute('PRAGMA user_version=99')
            if mode.startswith(('receipt-', 'cleanup-')) or mode in ('held', 'wait-handoff'):
                # Settled delivery with no disposition; only its original session can supply one.
                db.execute("INSERT INTO pane_roles VALUES ('socket','w','a','agent','')")
                db.execute("INSERT INTO pane_addresses VALUES ('socket','w','a','a',0,0,0)")
                db.execute("INSERT INTO pane_bindings VALUES ('socket','w','b','a','old:p',1,'old:t',0,NULL)")
                db.execute("INSERT INTO collaboration_threads VALUES ('socket','w','thread','history',0)")
                db.execute("INSERT INTO collaboration_messages VALUES ('socket','w','m','thread',1,NULL,'a','agent','human',NULL,'request','source','body','mention/v1',0)")
                db.execute("INSERT INTO message_recipients VALUES ('socket','w','m',1,'a','agent',NULL)")
                db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w','d','a',1,'m','settled',0)")
                db.execute("INSERT INTO delivery_attempts VALUES ('socket','w','attempt','d','a','b',?,NULL,0)",
                           ('' if mode == 'receipt-empty-session' else 'source-session',))
                db.execute("INSERT INTO delivery_attempt_state VALUES ('socket','w','attempt','a','settled',?)",
                           (int(mode == 'receipt-active'),))
                if mode in ('receipt-held', 'held', 'wait-handoff'):
                    db.execute("UPDATE mailbox_deliveries SET state='held'")
                    db.execute("UPDATE delivery_attempt_state SET state='held'")
                if mode == 'receipt-no-state': db.execute('DELETE FROM delivery_attempt_state')
                if mode.startswith('cleanup-'):
                    status = mode.removeprefix('cleanup-')
                    if status in ('cross-space', 'rollback', 'terminal', 'terminal-waiting'): status = 'submitted'
                    db.execute('UPDATE mailbox_deliveries SET state=?', (status,))
                    if status == 'queued':
                        db.execute('DELETE FROM delivery_attempt_state')
                        db.execute('DELETE FROM delivery_attempts')
                    else:
                        db.execute('UPDATE delivery_attempt_state SET state=?,active=?',
                                   (status, int(status in ('dispatching', 'submitted', 'acknowledged'))))
                    if mode == 'cleanup-rollback':
                        db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w','invalid','a',2,'m','submitted',0)")
                assert not db.execute('PRAGMA foreign_key_check').fetchall()
            if mode.startswith('join-'):
                # A complete, FK-valid historical fan-out: both deliveries have
                # terminal outcomes, but no joined result was materialized.
                for address in ('source', 'first', 'second'):
                    db.execute("INSERT INTO pane_roles VALUES ('socket','w',?,?, '')", (address, address))
                    db.execute("INSERT INTO pane_addresses VALUES ('socket','w',?,?, ?,?,0)",
                               (address, address, int(address == 'source' and mode == 'join-retired'),
                                int(address == 'source' and mode == 'join-disabled')))
                db.execute("INSERT INTO collaboration_threads VALUES ('socket','w','thread','history',0)")
                db.execute("""INSERT INTO collaboration_messages VALUES
                    ('socket','w','request','thread',1,NULL,'source','source','agent_session',?,
                     'request-id','source text','body','delegated/v1',0)""",
                           (None if mode == 'join-no-session' else 'source-session',))
                for order, (address, outcome) in enumerate((('first','failed'), ('second','cancelled')), 1):
                    db.execute("INSERT INTO message_recipients VALUES ('socket','w','request',?,?,?,NULL)",
                               (order, address, address))
                    db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w',?,?,1,'request',?,0)",
                               ('delivery-'+address, address, outcome))
                if mode == 'join-cleanup':
                    db.execute("UPDATE mailbox_deliveries SET state='queued'")
                    db.execute("INSERT INTO thread_controls VALUES ('socket','w','thread',0,0,10)")
                    db.execute("INSERT INTO order_counters VALUES ('socket','w','thread_message','thread',2)")
                assert not db.execute('PRAGMA foreign_key_check').fetchall()
            if mode.startswith('coordination-'):
                # An open handoff whose lead address was retired; no session can resolve it.
                db.execute("INSERT INTO pane_roles VALUES ('socket','w','lead','lead','')")
                db.execute("INSERT INTO pane_addresses VALUES ('socket','w','lead','lead',1,0,0)")
                db.execute("""INSERT INTO coordinations (session_socket_id,worktree_id,coordination_id,request_id,
                    owner_address_id,owner_session_id,deliverable,created_at_ms,updated_at_ms)
                    VALUES ('socket','w','orphan','request','lead','gone-session','Lead-loss probe',0,0)""")
                acknowledged = mode == 'coordination-acknowledged'
                db.execute("INSERT INTO attention_items VALUES ('socket','w','attention','coordination','orphan','lead retired',0,?)",
                           (1 if acknowledged else None,))
                if acknowledged:
                    db.execute("INSERT INTO transition_facts VALUES ('socket','w',1,'attention_item','attention','acknowledged','human','person',1)")
                assert not db.execute('PRAGMA foreign_key_check').fetchall()
            if mode.startswith('adopted-'):
                db.execute("INSERT INTO pane_roles VALUES ('socket','w','lead','lead','')")
                db.execute("INSERT INTO pane_addresses VALUES ('socket','w','lead','lead',0,0,0)")
                resolved = mode == 'adopted-resolved'
                if resolved:
                    db.execute("INSERT INTO collaboration_threads VALUES ('socket','w','report-thread','Report',0)")
                    db.execute("INSERT INTO collaboration_messages VALUES ('socket','w','report','report-thread',1,NULL,'lead','lead','agent_session','replacement','report','done','done','coordination-report/v1',0)")
                    db.execute("INSERT INTO message_recipients VALUES ('socket','w','report',1,'lead','lead','replacement')")
                    db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w','report-delivery','lead',1,'report','failed',0)")
                db.execute("""INSERT INTO coordinations (session_socket_id,worktree_id,coordination_id,request_id,
                    owner_address_id,owner_session_id,deliverable,outcome,report,report_message_id,created_at_ms,updated_at_ms)
                    VALUES ('socket','w','adopted','adopt-1','lead','replacement','Adopted probe',?,?,?,0,0)""",
                           ('completed', 'done', 'report') if resolved else ('open', None, None))
                db.execute("INSERT INTO transition_facts VALUES ('socket','w',1,'coordination_ownership','adopted:gone-session','adopted','agent_session','replacement',1)")
                assert not db.execute('PRAGMA foreign_key_check').fetchall()
            if mode == 'cleanup-cross-space':
                for table in ('worktrees','pane_roles','pane_addresses','pane_bindings', 'collaboration_threads',
                              'collaboration_messages','message_recipients','mailbox_deliveries',
                              'delivery_attempts','delivery_attempt_state'):
                    columns = [r[1] for r in db.execute('PRAGMA table_info('+table+')')][2:]
                    db.execute("INSERT INTO "+table+" SELECT 'old-socket','closed-space',"+','.join(columns)+" FROM "+table)
            terminal_schema = (REPO/'src/store/terminal.rs').read_text().split('const SCHEMA: &str = r#"', 1)[1].split('"#;', 1)[0]
            db.executescript(terminal_schema)
            if mode in ('cleanup-terminal', 'cleanup-terminal-waiting'):
                db.execute("INSERT INTO terminal_executions VALUES ('socket','w','attempt','term1','sleep 30',?,NULL,NULL,NULL)",
                           (None if mode == 'cleanup-terminal-waiting' else 1,))
            history_before = '\n'.join(db.iterdump())
        source = {'kind':'github', 'owner':'owner', 'repo':'repo', 'requested_ref':ref, 'resolved_commit':'a'*40,
                  'managed_path':str(base/'old root')}
        if mode == 'subdir-mismatch': source['subdir'] = 'wrong'
        if mode in ('local','archive'): source['kind'] = mode
        if mode == 'missing-ref': del source['requested_ref']
        (base/'registry').write_text(json.dumps({'result': {'type':'plugin_list', 'plugins':[
            {'plugin_id':'namedrop', 'plugin_root':str(old), 'source':source}]}}))
        env = {k:v for k,v in os.environ.items() if not k.startswith(('HERDR_', 'CODEX_'))}
        env['PYTHONPATH'] = str(base)
        env.update(PATH=str(base/'bin')+os.pathsep+os.environ['PATH'], FIXTURE=str(base), CASE=mode,
                   TEST_REPO=str(REPO), LIVE='1' if live else '0', INCOMPATIBLE='1' if incompatible else '',
                   HOME=str(base/'home'), CLAUDE_CONFIG_DIR=str(base/'home/.claude'), CODEX_HOME=str(base/'home/.codex'), HERDR_SOCKET_PATH='socket', HERDR_WORKSPACE_ID='w',
                   HERDR_PANE_ID='w:p', HERDR_PLUGIN_STATE_DIR=str(state))
        command = ['sh', str(old/'scripts/update.sh')]
        if mode == 'open': command.append('--open')
        output = b''
        if interactive:
            master, slave = pty.openpty()
            process = subprocess.Popen(command, env=env, cwd=old, stdin=slave, stdout=slave, stderr=slave)
            os.close(slave)
            chosen = closed = handled_wait = False
            answered = 0
            deadline = time.monotonic()+15
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try: chunk = os.read(master, 65536)
                    except OSError:
                        process.wait(timeout=2)
                        break
                    if not chunk:
                        process.wait(timeout=2)
                        break
                    output += chunk
                if b'Choose [1/2, default 2]:' in output and not chosen:
                    os.write(master, choice)
                    chosen = True
                if output.count(b'Install update now? [y/N]') > answered:
                    if mode == 'wait-reconfirm' and answered == 1:
                        assert not (base/'installed').exists(), 'Readiness must require a fresh confirmation'
                    os.write(master, answer if answered == 0 else second_answer)
                    answered += 1
                if b'Press Enter to do this later.' in output and not handled_wait:
                    assert not (base/'installed').exists(), 'Waiting must never install anything'
                    if waiting == b'\x03':
                        process.send_signal(signal.SIGINT)
                    elif waiting is not None:
                        os.write(master, waiting)
                    else:
                        # Simulate the user/agent finishing work independently.
                        # These changes are not made by the updater.
                        with sqlite3.connect(state/'namedrop.sqlite3') as db:
                            if mode == 'wait-handoff':
                                db.execute("UPDATE mailbox_deliveries SET state='settled'")
                                db.execute("UPDATE delivery_attempt_state SET state='settled',active=0")
                                db.execute("INSERT INTO delivery_dispositions VALUES ('socket','w','done','attempt','complete',1)")
                            if mode == 'wait-pause-change':
                                db.execute('UPDATE worktrees SET paused=0')
                            history_before = '\n'.join(db.iterdump())
                        if mode == 'wait-registry-change':
                            registry = json.loads((base/'registry').read_text())
                            registry['result']['plugins'][0]['source']['requested_ref'] = 'different'
                            (base/'registry').write_text(json.dumps(registry))
                        if mode != 'wait-chatter': (base/'finished').touch()
                    handled_wait = True
                if mode == 'wait-chatter' and handled_wait and int((base/'pane-count').read_text()) >= 3:
                    (base/'finished').touch()
                if b'Press Enter to close' in output and not closed:
                    os.write(master, b'\n')
                    closed = True
                if process.poll() is not None: break
            if process.poll() is None:
                process.kill()
                raise AssertionError('timeout: '+output.decode(errors='replace'))
            os.close(master)
            code = process.wait()
        else:
            process = subprocess.run(command, env=env, cwd=old, input=answer, capture_output=True, timeout=15)
            code = process.returncode
            output = process.stdout+process.stderr
        text = output.decode(errors='replace')
        calls = [json.loads(line) for line in (base/'calls').read_text().splitlines()] if (base/'calls').exists() else []
        commands = [c['args'] for c in calls]
        installs = [c for c in commands if c[:2] == ['plugin','install']]
        activates = [c for c in commands if c[:3] == ['plugin','action','invoke']]
        mutations = [c for c in commands if c[:2] in (
            ['pane','close'], ['workspace','close'], ['tab','close'], ['pane','send-keys'],
            ['agent','send-keys'], ['pane','run'], ['agent','prompt'])]
        assert not mutations, 'Updates must preserve panes and sessions: ' + repr(mutations)
        assert 'Clean up and update' not in text and 'Sessions to close' not in text, text
        assert not any('update-cleanup' in c for c in commands), commands
        cancelled = answer.strip().lower() not in (b'y', b'yes') or mode == 'wait-reconfirm' and second_answer.strip().lower() not in (b'y', b'yes')
        success_modes = ('success', 'unchanged', 'join-retired', 'join-disabled', 'join-no-session',
                         'schema-bump', 'schema-bump-layout', 'receipt-gone', 'join-gone',
                         'wait-finishes', 'wait-other-server', 'wait-handoff', 'wait-pause-change', 'other-server-idle', 'wait-chatter', 'wait-reconfirm', 'coordination-acknowledged', 'adopted-resolved')
        seamless = live and not legacy and not incompatible and codex_ready and prior_schema in (None, 3, 4, SCHEMA_VERSION)
        success = (mode in success_modes or seamless and mode in (
            'busy', 'busy-after-confirm', 'held', 'cleanup-queued', 'cleanup-held', 'cleanup-dispatching',
            'cleanup-submitted', 'cleanup-acknowledged', 'cleanup-terminal', 'cleanup-terminal-waiting',
            'join-cleanup', 'receipt-live', 'other-server', 'human-resume', 'live-drain-error')) and not cancelled and interactive
        waiting_mode = mode.startswith('wait-')
        if waiting_mode:
            assert handled_wait, text
            assert 'Leave this tab open; it will ask before installing.' in text, text
        if mode in ('busy', 'busy-after-confirm', 'wait-finishes') and ref.lower() != 'a'*40 and not seamless and not legacy and codex_ready:
            assert 'Research' in text and 'tab 2' in text and '@main' in text, text
            assert 'Reviewer' in text and 'working' in text and 'idle' in text, text
        if not codex_ready:
            assert "Finish Codex's one-time setup" in text and 'Reviewer' not in text, text
        if built in ('bin', 'target'):
            assert code == 0 and 'Active    matches installed version' in text and 'Already at your pinned revision' in text, text
            assert not installs and not activates and old.exists(), commands
        elif ref.lower() == 'a'*40 and interactive and not live:
            assert code == 0 and 'Already at your pinned revision' in text, text
            assert not installs and not activates and old.exists(), commands
            assert not any(c[:2] in (['pane','list'], ['session','list']) for c in commands), commands
        elif success:
            assert code == 0 and 'Update complete.' in text, text
            already_installed = mode == 'unchanged' or ref.lower() == 'a'*40
            if already_installed:
                assert not installs and old.exists(), commands
                assert 'Branch already installed' in text, text
            else:
                assert installs == [['plugin','install','owner/repo'+('/'+subdir if subdir else ''),'--ref',ref,'--yes']], commands
                assert not old.exists(), commands
                assert '[2/3] Install' in text, text
                i = commands.index(installs[0])
                assert commands[i+1] == ['plugin','list','--plugin','namedrop','--json'], commands
                assert all(not Path(c['cwd']).is_relative_to(old) for c in calls[i:]), calls
            assert len(activates) == 1 and '[3/3] Activate' in text, text
            assert ('Keep working in your current sessions' if seamless else 'Continue in your existing panes') in text, text
            assert 'Name replacement' not in text, text
            assert str(old) not in text and 'Manual recovery' not in text, text
            if mode == 'wait-reconfirm':
                assert answered == 2, text
            elif waiting_mode:
                assert text.index('Press Enter to do this later.') < text.index('Install update now?'), text
            assert 'Installed ' in text and 'Active ' in text, text
            if prior_schema is not None:
                assert f'schema {prior_schema}; installed version awaits activation' in text, text
            if mode == 'live-drain-error':
                assert 'fixture reconciliation failure' in text and 'codex plugin add namedrop@namedrop --json' in text, text
            if mode.startswith('schema-bump'):
                assert 'Pause preservation is unverified' in text and 'Update stopped' not in text, text
                assert str(new/'scripts/setup.sh') in text and 'check --human' in text, text
        elif mode in ('pause-change', 'schema-bump-pause-change', 'plugin-resume'):
            assert code != 0 and 'Installation and activation succeeded' in text and 'Pause verification failed' in text, text
        elif mode == 'install-fail':
            assert code != 0 and len(installs) == 1 and not activates and 'activation was not attempted' in text, text
            assert '--ref '+('a'*40) in text, text
        elif mode == 'activation-fail':
            assert code != 0 and len(installs) == len(activates) == 1 and 'Reinstall succeeded but activation' in text, text
        elif mode == 'open':
            assert code == 0 and len(commands) == 1 and commands[0][:3] == ['plugin','pane','open'], text
        else:
            assert not installs and not activates, commands
            stopped = mode in ('malformed','local','archive','missing-ref','subdir-mismatch','schema',
                               'wait-registry-change','wait-broken-query') or not interactive
            assert (code != 0) == stopped, text
            if not stopped:
                assert 'Update postponed. Your work is unchanged' in text, text
            if mode == 'busy-after-confirm':
                assert 'Work resumed. Installation will wait.' in text, text
            if mode == 'coordination-orphan':
                assert 'Lead-loss probe (coordination:open)' in text, text
            if mode == 'adopted-open':
                assert 'Adopted probe (coordination:open)' in text, text
            if mode in ('receipt-returned', 'join-returned'):
                assert 'Install update now? [y/N]' in text, text
            if mode == 'malformed':
                assert 'herdr plugin list --plugin namedrop --json' in text and 'manual-upgrade' in text, text
            if mode == 'wait-registry-change':
                assert 'Installed source/ref changed' in text, text
            if mode == 'wait-broken-query':
                assert 'Cannot inspect panes' in text, text
        if mode == 'wait-chatter':
            assert text.count('Waiting for current work.') == 1, 'Unchanged blockers must not flood the tab: ' + text
        with sqlite3.connect(state/'namedrop.sqlite3') as db:
            if mode not in ('pause-change', 'schema-bump', 'schema-bump-layout', 'schema-bump-pause-change', 'human-resume', 'plugin-resume'):
                assert '\n'.join(db.iterdump()) == history_before, 'Update must preserve ALL collaboration records'
            table = 'migrated_worktrees' if mode == 'schema-bump-layout' else 'worktrees'
            expected = 0 if mode in ('pause-change', 'schema-bump-pause-change', 'wait-pause-change', 'human-resume', 'plugin-resume') else 1
            assert db.execute('SELECT paused FROM '+table).fetchall() == [(expected,)] * (2 if mode == 'cleanup-cross-space' else 1)
            assert not db.execute("SELECT 1 FROM transition_facts WHERE actor_id='guided-update'").fetchall()
        print(mode, ref, 'choice='+repr(choice), repr(answer), 'PASS')
        return text


if __name__ == '__main__':
    for pin in ('main','v1.0.0','a'*40,'c'*40):
        case('success', ref=pin, subdir='plugins/namedrop')
    for mode in ('malformed','local','archive','missing-ref','subdir-mismatch','busy','busy-after-confirm','held','uncertain',
                 'other-server','other-server-idle','other-server-unavailable','schema','install-fail','activation-fail','label-fallback'):
        case(mode)
    case('success', answer=b'no\n')
    case('success', answer=b'\x04')
    case('success', interactive=False)
    case('open', interactive=False)
    case('unchanged')
    for mode in ('join-retired', 'join-disabled', 'join-no-session', 'join-eligible',
                 'schema-bump', 'schema-bump-layout', 'pause-change', 'schema-bump-pause-change',
                 'receipt-gone', 'receipt-live', 'receipt-unknown', 'receipt-returned', 'receipt-active',
                 'receipt-empty-session', 'receipt-no-state', 'receipt-held', 'join-gone', 'join-unknown', 'join-returned'):
        case(mode)
    for mode in ('coordination-orphan', 'coordination-acknowledged', 'adopted-open', 'adopted-resolved'):
        case(mode)
    # All delivery states, closed spaces and shell commands remain intact on deferral.
    for mode in ('cleanup-queued', 'cleanup-held', 'cleanup-dispatching', 'cleanup-submitted', 'cleanup-acknowledged',
                 'cleanup-cross-space', 'cleanup-terminal', 'cleanup-terminal-waiting', 'join-cleanup'):
        case(mode, choice=b'1\n')
    for mode in ('wait-finishes', 'wait-other-server', 'wait-handoff', 'wait-pause-change',
                 'wait-registry-change', 'wait-broken-query', 'wait-chatter'):
        case(mode, choice=b'1\n', waiting=None)
    for choice in (b'2\n', b'\n', b'\x04'):
        case('busy', choice=choice)
    for waiting in (b'\n', b'\x04', b'\x03'):
        case('busy', choice=b'1\n', waiting=waiting)
    case('busy', choice=b'invalid\n1\n')

    case('busy', ref='a'*40)
    case('held', ref='A'*40)
    case('wait-reconfirm', choice=b'1\n', waiting=None)
    case('wait-reconfirm', choice=b'1\n', waiting=None, second_answer=b'n\n')
    for mode in ('busy', 'busy-after-confirm', 'held', 'cleanup-queued', 'cleanup-held', 'cleanup-dispatching',
                 'cleanup-submitted', 'cleanup-acknowledged', 'cleanup-terminal', 'cleanup-terminal-waiting',
                 'join-cleanup', 'receipt-live', 'other-server'):
        case(mode, live=True)
    case('busy', live=True, answer=b'n\n')
    case('busy', live=True, legacy=True)
    case('busy', live=True, incompatible=True)
    case('other-server-unavailable', live=True)
    case('success', ref='a'*40, live=True)
    for built in ('bin', 'target', 'stale-bin'):
        case('success', ref='a'*40, live=True, built=built)
    case('human-resume', live=True)
    case('plugin-resume', live=True)
    case('busy', live=True, codex_ready=False)
    case('live-drain-error', live=True)
    # Every declared live upgrade source keeps live sessions, held work and
    # original records intact; anything outside that set refreshes sessions.
    case('busy', live=True, prior_schema=3)
    case('held', live=True, prior_schema=3)
    case('receipt-live', live=True, prior_schema=3)
    case('busy', live=True, prior_schema=4)
    case('busy', live=True, prior_schema=2)
    case('busy', live=True, prior_schema=SCHEMA_VERSION + 1)
    case('unchanged', live=True, prior_schema=3)
    print('update lifecycle checks passed')
