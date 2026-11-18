"""U6 on disposable Rex servers: the module, `rex action`, the guarded start,
and Doctor's Rex report.

Usage: python3 -I tests/rex_ui_live.py BINARY REX_BINARY OUT

Each server runs under OUT's own HOME, on a socket under /tmp, with
`--autostart=false -S unix://...` on every Rex call. Herdr is a stub that
only sleeps, so a pane can have `herdr` as its foreground. Claude and Codex
are never started. Every process left under the fixture's folder is stopped
by pid at the end.

1. The owner's activation renders the registered module (a fixture rex.json).
2. A server whose config's first line is the require line: its start runs one
   bridge, which `config check` and two reloads leave alone; `rex actions`
   lists eleven actions; `namedrop_about` names the module, the baked
   launcher and state, and the server's HOME and REX_SERVER; Doctor confirms
   it with five keys in effect; palette activation opens one layer and starts
   the deferred-command worker (the U6 review's F6); `namedrop_name` opens
   the name screen; a refused action prints `Error: <one line>`; a later
   binding, and one to an action no file defines, shadow their keys (F5); a
   rex.json that About disagrees with is not confirmed; and a module the
   server loaded before a refresh rewrote it asks for a reload, which
   confirms it (F7).
3. A server whose REX_SERVER spells its socket through another folder than
   its listener runs one bridge, keyed by the listener, before and after an
   action (F1).
"""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

namedrop, rexbin, out = (Path(p).resolve() for p in sys.argv[1:4])
out.mkdir(parents=True, exist_ok=True)
root = Path(tempfile.mkdtemp(prefix='nd-u6-', dir='/tmp')).resolve()
home, state = root / 'home', root / 'state'
home.mkdir()
cfgdir = home / '.config' / 'rex'
cfgdir.mkdir(parents=True)
init, module = cfgdir / 'init.lua', cfgdir / 'namedrop.lua'
record = home / '.config' / 'namedrop' / 'rex.json'
record.parent.mkdir(parents=True)
LINE = 'pcall(require, "namedrop");\n'
PERSON = '-- the person\'s own config\nrex.action{ name = "mine", title = "Mine", run = function() return "mine" end }\n'
init.write_text(LINE + PERSON)
record.write_text(json.dumps({'module': str(module), 'config': str(init), 'line': 'inserted'}))
# The state is standalone, so its bridge would start the update check.
(record.parent / 'settings.json').write_text('{"check_for_updates": false}\n')
base_env = {'HOME': str(home), 'PATH': '/usr/bin:/bin', 'USER': os.environ.get('USER', 'u'), 'SHELL': '/bin/sh',
            'LANG': 'en_US.UTF-8'}
digest = hashlib.sha256(namedrop.read_bytes()).hexdigest()
release = state / 'runtime' / 'releases' / digest
release.mkdir(parents=True)
shutil.copy2(namedrop, release / 'namedrop')
shutil.copy2(Path(__file__).resolve().parents[1] / 'runtime.json', release / 'runtime.json')
(state / 'runtime' / 'current').symlink_to(Path('releases') / digest)
(state / 'runtime' / 'owner').write_text('standalone')
launcher = state / 'runtime' / 'namedrop'
launcher.write_text('#!/bin/sh\nexec "$(dirname "$0")/current/namedrop" "$@"\n')
launcher.chmod(0o700)
fg = root / 'fg'
fg.mkdir()
(fg / 'herdr.c').write_text('#include <unistd.h>\nint main(void){sleep(600);return 0;}\n')
subprocess.run(['/usr/bin/cc', '-o', str(fg / 'herdr'), str(fg / 'herdr.c')], check=True)
ev = {'root': str(root), 'binary_sha256': digest, 'checks': {}, 'observed': {}}
servers = []


def wait_for(read, ok, what, t=15):
    end = time.monotonic() + t
    v = None
    while time.monotonic() < end:
        v = read()
        if ok(v):
            return v
        time.sleep(0.1)
    raise AssertionError(what + ': ' + repr(v))


def start(tag, listen=None, named=None):
    """A server on `listen`, whose REX_SERVER names `named`, both its socket by default."""
    sock = listen or root / (tag + '.sock')
    log = (out / f'server-{tag}.log').open('w')
    p = subprocess.Popen([str(rexbin), '--autostart=false', '-S', 'unix://' + str(sock), 'server', 'run', '--listen',
                          'unix://' + str(sock), '--state-dir', str(root / ('rex-state-' + tag)), '--restore=false'],
                         env={**base_env, 'REX_SERVER': 'unix://' + str(named or sock)}, stdout=log,
                         stderr=subprocess.STDOUT)
    servers.append((p, log))
    wait_for(lambda: sock.exists() or p.poll() is not None, bool, 'server start ' + tag)
    assert p.poll() is None, 'server exited'
    return p, sock


def stop(p):
    if p.poll() is None:
        p.terminate()
        p.wait(timeout=10)


def rex(sock, *a, check=True, timeout=30):
    r = subprocess.run([str(rexbin), '--autostart=false', '-S', 'unix://' + str(sock), *a], env=base_env, text=True,
                       capture_output=True, timeout=timeout)
    if check and r.returncode:
        raise AssertionError((a, r.returncode, r.stdout, r.stderr))
    return r


def procs():
    listing = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,command='], capture_output=True, text=True).stdout
    rows = []
    for line in listing.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3:
            rows.append((int(parts[0]), int(parts[1]), parts[2]))
    return rows


def bridges(*socks):
    """The bridges of the server at any of these spellings of its socket."""
    keys = tuple('rex:' + str(s) for s in socks)
    return sorted((pid, ppid, cmd.rstrip().rsplit(' ', 1)[-1]) for pid, ppid, cmd in procs()
                  if ' rex bridge --endpoint ' in cmd and cmd.rstrip().endswith(keys))


def deferred_workers(sock):
    return [pid for pid, _, cmd in procs() if cmd.rstrip().endswith(' --deferred-worker ' + str(state) + ' rex:' + str(sock))]


def one_bridge(what, *socks):
    """Waits for a bridge, as a cold binary takes a while to start one, then
    gives a second one time to appear."""
    wait_for(lambda: bridges(*socks), bool, what + ': no bridge')
    time.sleep(2)
    found = bridges(*socks)
    ev['observed'][what] = found
    assert len(found) == 1, (what, found)
    return found[0]


def nd(sock, S, A, *a, check=True):
    r = subprocess.run([str(launcher), '--endpoint', 'rex:' + str(sock), '--space', S, '--pane', A, '--json', *a],
                       env={**base_env, 'NAMEDROP_STATE_DIR': str(state)}, text=True, capture_output=True, timeout=60)
    if check and r.returncode:
        raise AssertionError((a, r.returncode, r.stdout, r.stderr))
    return r


def doctor_rex(sock, S, A):
    r = nd(sock, S, A, 'doctor', check=False)
    data = json.loads(r.stdout)['data']
    issue = [i for i in data['issues'] if i.get('code') == 'ND-REX-001']
    return r, data, (issue[0] if issue else None)


def session(sock, name):
    created = json.loads(rex(sock, 'new', name, '--shell', 'none', '--json', '--', '/bin/sh', '-i').stdout)
    return created['session_id'], created['initial_windows'][0]['block_ids'][0]


def api(sock, method, data):
    return json.loads(rex(sock, 'api', 'call', method, json.dumps(data)).stdout)


def blocks(sock, S):
    return {b['block_id']: b for b in api(sock, 'session.list_blocks', {'session_id': S})['blocks']}


def capture(sock, S, b):
    return rex(sock, 'capture', '-s', S, '-b', b, '--trim', check=False).stdout


def about_of(sock):
    reply = json.loads(rex(sock, 'do', 'namedrop_about', '--json').stdout)
    return reply.get('result', reply) if isinstance(reply, dict) and 'module' not in reply else reply


def reload(sock):
    r = rex(sock, 'config', 'reload', check=False)
    assert r.returncode == 0, r


try:
    # 1. The owner's activation renders the registered module.
    pA, sockA = start('a')
    ev['version'] = rex(sockA, 'version').stdout
    S1, A1 = session(sockA, 'u6-render')
    assert not module.exists()
    r = nd(sockA, S1, A1, 'activate', '--input-json', '{"confirmed":true}')
    ev['observed']['phase1_activate_shortcuts'] = json.loads(r.stdout)['data'].get('shortcuts')
    text = module.read_text()
    assert text.startswith('-- Managed by Namedrop.') and f'local NAMEDROP = "{launcher}"' in text, text[:300]
    saved = json.loads(record.read_text())
    assert saved['launcher'] == str(launcher) and saved['state'] == str(state) and saved['line'] == 'inserted', saved
    (out / 'namedrop.lua').write_text(text)
    ev['checks']['owner activation rendered the registered module and recorded launcher/state'] = True
    stop(pA)
    with contextlib.suppress(AssertionError):
        wait_for(lambda: bridges(sockA), lambda b: not b, 'server A\'s bridge outlived its server', t=20)
    assert init.read_text() == LINE + PERSON, 'config edited'

    # 2. A fresh server loads the module at start.
    pB, sock = start('b')
    bridge = one_bridge('after_start', sock)
    ev['checks'][f'server start ran one bridge (pid {bridge[0]}, parent {bridge[1]}, server {pB.pid})'] = True
    chk = rex(sock, 'config', 'check', check=False)
    (out / 'config-check.txt').write_text(f'exit={chk.returncode}\n{chk.stdout}\n{chk.stderr}')
    assert chk.returncode == 0, chk
    assert one_bridge('after_config_check', sock) == bridge, 'config check changed the bridge'
    for n in (1, 2):
        reload(sock)
        assert one_bridge(f'after_reload_{n}', sock) == bridge, f'reload {n} changed the bridge'
    ev['checks']['config check and two config reloads kept the one bridge'] = True
    acts = json.loads(rex(sock, 'actions', '--json').stdout)
    (out / 'actions.json').write_text(json.dumps(acts, indent=2))
    items = acts if isinstance(acts, list) else acts.get('actions', acts)
    names = sorted(a['name'] for a in items if a['name'].startswith('namedrop_'))
    assert len(names) == 11, names
    ev['checks']['rex actions lists eleven namedrop_ actions'] = True
    about = about_of(sock)
    (out / 'about.json').write_text(json.dumps(about, indent=2))
    assert about['module'].lstrip('@') == str(module), about
    assert about['launcher'] == str(launcher) and about['state'] == str(state), about
    assert about['env']['HOME'] == str(home) and about['env']['REX_SERVER'] == 'unix://' + str(sock), about
    ev['checks']['namedrop_about returned the absolute module, baked launcher/state and server HOME/REX_SERVER'] = True

    S, A = session(sock, 'u6')
    r, data, issue = doctor_rex(sock, S, A)
    (out / 'doctor-confirmed.json').write_text(json.dumps(data, indent=2))
    assert issue and issue['severity'] == 'info' and issue['diagnostic']['confirmed'] is True, issue
    keys = {k['key']: k for k in issue['diagnostic']['keys']}
    assert len(keys) == 5 and all(k['status'] == 'in_effect' for k in keys.values()), keys
    assert issue['diagnostic']['env']['HOME'] == str(home), issue
    assert 'null' not in issue['explanation'], issue['explanation']
    ev['checks']['Doctor: ND-REX-001 confirmed, five keys in effect, server env from About'] = True

    # Palette activation opens one layer, Doctor, as no agent is present, and
    # starts the deferred-command worker, as the CLI's activation does.
    assert not deferred_workers(sock), 'a deferred worker ran before activation'
    before = set(blocks(sock, S))
    act = rex(sock, 'do', '-s', S, '-b', A, '--origin', 'palette', 'namedrop_activate', check=False)
    (out / 'do-activate.txt').write_text(f'exit={act.returncode}\n{act.stdout}\n{act.stderr}')
    wait_for(lambda: set(blocks(sock, S)) - before, bool, 'activation opened no layer')
    time.sleep(1.5)
    layers = set(blocks(sock, S)) - before
    assert len(layers) == 1, layers
    layer = next(iter(layers))
    text = wait_for(lambda: capture(sock, S, layer),
                    lambda t: 'The Rex module is confirmed.' in t and 'Loaded' in t and '\\n' not in t, 'Doctor did not paint')
    (out / 'activate-layer.txt').write_text(text)
    rex(sock, 'send-key', '-s', S, '-b', layer, 'escape')
    wait_for(lambda: blocks(sock, S), lambda bs: layer not in bs, 'Doctor layer stayed open')
    workers = wait_for(lambda: deferred_workers(sock), bool, 'palette activation started no deferred worker')
    ev['observed']['deferred_workers'] = workers
    ev['checks'][f'palette activation (exit {act.returncode}) opened one layer, Doctor, and started the deferred worker'] = True
    r = nd(sock, S, A, 'activate', '--input-json', '{"confirmed":true}')
    sc = json.loads(r.stdout)['data']['shortcuts']
    assert sc['status'] == 'rex_module' and sc['path'] == str(module) and len(sc['keys']) == 5, sc
    ev['checks']['CLI activate reports shortcuts rex_module with the module path and five keys'] = True

    # The name screen from the palette.
    before = set(blocks(sock, S))
    nm = rex(sock, 'do', '-s', S, '-b', A, '--origin', 'palette', 'namedrop_name', check=False)
    layer = next(iter(wait_for(lambda: set(blocks(sock, S)) - before, bool, 'namedrop_name opened no layer')))
    text = wait_for(lambda: capture(sock, S, layer), lambda t: 'Name this pane' in t and 'Mention name: @' in t,
                    'name screen did not paint')
    (out / 'name-layer.txt').write_text(text)
    rex(sock, 'send', '-s', S, '-b', layer, 'probe-u6')
    rex(sock, 'send-key', '-s', S, '-b', layer, 'enter')
    rex(sock, 'send-key', '-s', S, '-b', layer, 'enter')
    wait_for(lambda: blocks(sock, S), lambda bs: layer not in bs, 'name layer stayed open')
    label = wait_for(lambda: blocks(sock, S)[A].get('label'), lambda l: l == '@probe-u6', 'A not labelled')
    ev['checks'][f'rex do namedrop_name --origin palette (exit {nm.returncode}) painted Name this pane; saved {label}'] = True

    # A refused action prints Error: <one line>.
    C = json.loads(rex(sock, 'run', '-s', S, '--split', 'right', '--shell', 'none', '--focus=false', '--json', '--',
                       str(fg / 'herdr')).stdout)['block_ids'][0]
    wait_for(lambda: api(sock, 'com.superlogical.terminal.process', {'session_id': S, 'block_id': C, 'args': {}}),
             lambda f: (f.get('foreground') or {}).get('name') == 'herdr', 'herdr foreground not seen')
    before = set(blocks(sock, S))
    bad = rex(sock, 'do', '-s', S, '-b', C, '--origin', 'palette', 'namedrop_name', check=False)
    (out / 'do-failing.txt').write_text(f'exit={bad.returncode}\nstdout={bad.stdout!r}\nstderr={bad.stderr!r}\n')
    msg = (bad.stdout + bad.stderr).strip()
    assert bad.returncode != 0, bad
    assert msg == "Error: This pane is running Herdr. Use Herdr's own Name this pane.", msg
    time.sleep(1)
    assert set(blocks(sock, S)) == before, 'refusal opened a layer'
    ev['checks']['failing action printed one line: ' + msg] = True

    # A later binding shadows a Namedrop key, and so does one to an action no
    # file defines; Doctor names each source line.
    init.write_text(init.read_text() + 'rex.bind("alt+shift+cmd+m", "mine")\nrex.bind("alt+shift+cmd+s", "no_such_action")\n')
    reload(sock)
    assert one_bridge('after_reload_3', sock) == bridge, 'third reload changed the bridge'
    r, data, issue = doctor_rex(sock, S, A)
    (out / 'doctor-shadowed.json').write_text(json.dumps(data, indent=2))
    keys = {k['key']: k for k in issue['diagnostic']['keys']}
    for key, action in [('alt+shift+cmd+m', 'mine'), ('alt+shift+cmd+s', 'no_such_action')]:
        k = keys[key]
        assert k['status'] == 'shadowed' and k['effective_action'] == action and 'init.lua' in (k['source'] or ''), k
    assert all(k['status'] == 'in_effect' for key, k in keys.items() if key not in ('alt+shift+cmd+m', 'alt+shift+cmd+s')), keys
    ev['checks']['a later binding, and one to no defined action, are reported shadowed at their lines'] = True

    # A rex.json that About disagrees with is not confirmed.
    good = record.read_text()
    bad_record = json.loads(good)
    bad_record['state'] = str(root / 'elsewhere')
    record.write_text(json.dumps(bad_record))
    r, data, issue = doctor_rex(sock, S, A)
    record.write_text(good)
    (out / 'doctor-mismatch.json').write_text(json.dumps(data, indent=2))
    assert issue['diagnostic']['confirmed'] is False and issue['severity'] != 'info', issue
    assert 'scripts/install.sh' in issue['repair']['prompt'], issue
    ev['checks']['Doctor reports a module whose About disagrees with rex.json as not confirmed'] = True

    # A module the server loaded before a refresh rewrote it needs a reload.
    current = module.read_text()
    module.write_text(current.replace(f'local NAMEDROP = "{launcher}"', f'local NAMEDROP = "{root / "older/namedrop"}"'))
    reload(sock)
    assert about_of(sock)['launcher'] == str(root / 'older/namedrop')
    r = nd(sock, S, A, 'activate', '--input-json', '{"confirmed":true}')
    assert module.read_text() == current, 'the refresh did not rewrite the module'
    sc = json.loads(r.stdout)['data']['shortcuts']
    assert sc == {'status': 'stale_module', 'path': str(module),
                  'repair': "Reload Rex's configuration with rex config reload."}, sc
    r, data, issue = doctor_rex(sock, S, A)
    (out / 'doctor-stale.json').write_text(json.dumps(data, indent=2))
    assert issue['diagnostic']['confirmed'] is False and 'older namedrop.lua' in issue['explanation'], issue
    assert issue['repair']['prompt'] == "Reload Rex's configuration with rex config reload. Then check Namedrop Doctor again.", issue
    reload(sock)
    r, data, issue = doctor_rex(sock, S, A)
    assert issue['diagnostic']['confirmed'] is True, issue
    ev['checks']['a stale loaded module asks for a reload, and the reload confirms it'] = True
    assert bridges(sock)[0] == bridge
    stop(pB)

    # 3. REX_SERVER names the socket through another folder than the
    # listener's: one bridge, keyed by the listener, which the action finds.
    real = root / 'real'
    real.mkdir()
    (root / 'alias').symlink_to(real)
    pC, sockC = start('c', listen=real / 'c.sock', named=root / 'alias' / 'c.sock')
    aliased = root / 'alias' / 'c.sock'
    found = one_bridge('alias_after_start', sockC, aliased)
    assert found[2] == 'rex:' + str(sockC), found
    SC, AC = session(sockC, 'u6-alias')
    before = set(blocks(sockC, SC))
    act = rex(sockC, 'do', '-s', SC, '-b', AC, '--origin', 'palette', 'namedrop_doctor', check=False)
    layer = next(iter(wait_for(lambda: set(blocks(sockC, SC)) - before, bool, 'namedrop_doctor opened no layer')))
    rex(sockC, 'send-key', '-s', SC, '-b', layer, 'escape')
    assert one_bridge('alias_after_action', sockC, aliased) == found, 'the action started a second bridge'
    ev['checks']['a REX_SERVER spelled through an alias runs one bridge, keyed by the listener'] = True
    ev['passed'] = True
finally:
    for p, log in servers:
        try:
            stop(p)
        except Exception as e:  # noqa
            ev.setdefault('errors', []).append(repr(e))
        log.close()
    time.sleep(2)
    prefixes = (str(root) + '/',)
    stray = [(pid, cmd) for pid, _, cmd in procs() if cmd.startswith(prefixes) or (' ' + str(root) + '/') in cmd]
    ev['stray'] = stray
    for pid, _ in stray:
        with contextlib.suppress(ProcessLookupError):
            os.kill(pid, signal.SIGTERM)
    time.sleep(1)
    ev['stray_after_kill'] = [(pid, cmd) for pid, _, cmd in procs() if cmd.startswith(prefixes)]
    (out / 'report.json').write_text(json.dumps(ev, indent=2) + '\n')
    print(json.dumps(ev, indent=2))
    if not ev['stray_after_kill']:
        shutil.rmtree(root, ignore_errors=True)
sys.exit(0 if ev.get('passed') else 1)
