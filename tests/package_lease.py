"""Exercise every package writer through an installed runtime and a script host."""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

repo=Path(__file__).resolve().parents[1]
binary=Path(sys.argv[1]).resolve()
WRITERS={
    'src/install.rs::install_integration_package_with_config': ['activate','event','hook'],
    'src/install.rs::remove_subagent_hooks':['deactivate'],
    'src/install.rs::replace_if_changed':['activate','name','deactivate','event','hook'],
    'src/install.rs::replace_with_status':['activate','name','deactivate','event','hook'],
    'src/install/command.rs::install_shell_command':['activate'],
    'src/install/codex_sandbox.rs::activate':['activate','name'],
    'src/install/codex_sandbox.rs::remove':['deactivate'],
    'src/install/codex_sandbox.rs::remove_owned':['deactivate'],
    'src/install/codex_sandbox.rs::save':['activate','name','deactivate'],
    'src/install/codex_sandbox.rs::recover':['activate','name','deactivate'],
    'src/install/codex_sandbox.rs::publish':['activate','name','deactivate'],
    'src/install/claude_permissions.rs::refresh':['activate','event','hook'],
    'src/install/claude_permissions.rs::remove_claude_permissions':['deactivate'],
    'src/install/claude_permissions.rs::write_record':['activate','deactivate','event','hook'],
    'src/install/claude_permissions.rs::publish':['activate','deactivate','event','hook'],
    'src/install/claude_permissions.rs::write':['activate','deactivate','event','hook'],
    'src/install/claude_permissions.rs::rollback':['activate','deactivate','event','hook'],
    'src/install/claude_permissions.rs::recover':['activate','deactivate','event','hook'],
    'src/install/claude_permissions.rs::discard_pending':['activate','deactivate','event','hook'],
    'src/locator.rs::write_install_record':['activate','name','event','hook'],
}
found=set()
for path in (repo/'src').rglob('*.rs'):
    text=path.read_text().split('#[cfg(test)]\nmod ',1)[0]
    for name in re.findall(r"\bfn\s+(\w+)(?:\s*<[^{}]*?>)?\s*\([^{};]*?&(?:'[_a-zA-Z][_a-zA-Z0-9]*\s+)?(?:\w+::)*PackageLease\b",text,re.S):
        found.add(str(path.relative_to(repo))+'::'+name)
assert found==set(WRITERS),{'unmapped':sorted(found-set(WRITERS)),'stale':sorted(set(WRITERS)-found)}

def snapshot(home):
    targets=['.agents/skills/namedrop','.claude/skills/namedrop','.claude/settings.json','.codex/rules','.codex/config.toml','.local/bin/namedrop','.config/namedrop/state-directory','.profile','.bashrc']
    result={}
    for name in targets:
        path=home/name
        paths=sorted(path.rglob('*')) if path.is_dir() else [path]
        for file in paths:
            if file.is_file():result[str(file.relative_to(home))]=hashlib.sha256(file.read_bytes()).hexdigest()
    return result

with tempfile.TemporaryDirectory(prefix='nd-lease-matrix-') as folder:
    root=Path(folder)
    (root/"repo").mkdir()
    sock=socket.socket(socket.AF_UNIX);address=str(root/'h.sock');sock.bind(address);sock.listen()
    def accept():
        while True:
            try:conn,_=sock.accept();conn.close()
            except OSError:return
    threading.Thread(target=accept,daemon=True).start()
    def installed(name,owner):
        state=root/name;runtime=state/'runtime';digest=hashlib.sha256(binary.read_bytes()).hexdigest();release=runtime/'releases'/digest
        release.mkdir(parents=True)
        import shutil
        shutil.copy2(binary,release/'namedrop');shutil.copy2(repo/'runtime.json',release/'runtime.json')
        (runtime/'current').symlink_to('releases/'+digest)
        (runtime/'owner').write_text(owner)
        launcher=runtime/'namedrop'
        launcher.write_text('#!/usr/bin/python3 -IB\nimport json,os,sys\nwith open('+repr(str(root/'pids'))+',"a") as f:f.write(json.dumps({"pid":os.getpid(),"args":sys.argv})+"\\n")\nos.execv('+repr(str(runtime/'current/namedrop'))+',sys.argv)\n')
        launcher.chmod(0o755)
        return state
    old=installed('herdr','herdr')
    state=installed('standalone','standalone')
    inactive=installed('inactive','standalone')
    def env(home,selected,pane='p1'):
        home.mkdir(exist_ok=True)
        return {'HOME':str(home),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','ROOT':str(root),'CHECKOUT':str(root/'repo'),'SHELLPID':str(os.getpid()),'SOCK':address,'PYTHONDONTWRITEBYTECODE':'1','HERDR_BIN_PATH':str(repo/'tests/differential/herdr'),'HERDR_SOCKET_PATH':address,'HERDR_WORKSPACE_ID':'w1','HERDR_PANE_ID':pane,'HERDR_PLUGIN_STATE_DIR':str(selected)}
    def run(home,selected,args,extra=None):
        e=env(home,selected,'p3' if args[0]=='name' else 'p1');e.update(extra or {})
        p=subprocess.run([str(selected/'runtime/namedrop'),*args],input='',cwd=root/'repo',env=e,capture_output=True,text=True,timeout=30)
        return p
    activate=['--json','activate','--input-json','{"confirmed":true}']
    p=run(root/'prewarm',state,activate);assert p.returncode==0,(p.stdout,p.stderr)
    cases={
        'activate':activate,
        'name':['name','lease-name','--type','terminal'],
        'deactivate':['--json','deactivate'],
        'event':['event','agent-status-changed','--input-json',json.dumps({'type':'pane_agent_status_changed','pane_id':'p1','workspace_id':'w1','agent_status':'idle'})],
        'hook':['hook','agent-status-changed'],
    }
    extra={'HERDR_PLUGIN_EVENT':'pane.agent_status_changed','HERDR_PLUGIN_EVENT_JSON':json.dumps({'event':'pane_agent_status_changed','data':{'type':'pane_agent_status_changed','pane_id':'p1','workspace_id':'w1','agent_status':'idle'}})}
    try:
        home=root/'home';p=run(home,old,activate);assert p.returncode==0,(p.stdout,p.stderr)
        for name,args in cases.items():
            before=snapshot(home);p=run(home,inactive if name=='name' else state,args,extra)
            assert 'ND-INSTALL-002' in p.stdout+p.stderr,(name,p.returncode,p.stdout,p.stderr)
            assert snapshot(home)==before,('denied writer changed files',name)
            print('denied',name,'unchanged',flush=True)
        for name,args in cases.items():
            own=root/('own-'+name)
            record=own/'.config/namedrop/state-directory';record.parent.mkdir(parents=True,exist_ok=True)
            selected=inactive if name=='name' else state
            record.write_text(str(selected)+'\n')
            if name not in ['name','activate']:
                p=run(own,state,activate);assert p.returncode==0,(name,p.stdout,p.stderr)
            if name in ['event','hook']:
                skill=own/'.agents/skills/namedrop/SKILL.md';expected_skill=skill.read_text();skill.write_text('stale package\n')
            before=snapshot(own);time.sleep(.6)
            p=run(own,inactive if name=='name' else state,args,extra)
            assert 'ND-INSTALL-002' not in p.stdout+p.stderr,(name,p.stdout,p.stderr)
            assert p.returncode==0,(name,p.stdout,p.stderr)
            if name=='hook':
                until=time.monotonic()+10
                while snapshot(own)==before and time.monotonic()<until:time.sleep(.05)
            after=snapshot(own)
            assert after!=before,('admitted writer changed no shared file',name,p.stdout,p.stderr)
            if name in ['activate','name']:
                for package in ['.agents/skills/namedrop','.claude/skills/namedrop']:
                    assert (own/package/'SKILL.md').is_file(),(name,package)
                    assert str(selected) in (own/package/'scripts/session-start').read_text(),name
                assert 'deferred-commands' in (own/'.codex/config.toml').read_text(),name
                if name=='activate':assert str(selected/'runtime/namedrop') in (own/'.local/bin/namedrop').read_text()
            elif name=='deactivate':
                assert 'deferred-commands' not in (own/'.codex/config.toml').read_text()
            else:
                assert skill.read_text()==expected_skill,(name,'package refresh did not restore skill')
            print('admitted',name,'wrote files',flush=True)
    finally:
        sock.close()
        if (root/'pids').exists():
            for line in (root/'pids').read_text().splitlines():
                entry=json.loads(line)
                if any(arg=='reminder-clock' or arg.endswith('worker') or arg=='serve' for arg in entry['args']):
                    try:os.kill(entry['pid'],signal.SIGTERM)
                    except ProcessLookupError:pass
            time.sleep(.3)
print('writer inventory and entry point matrix passed')
