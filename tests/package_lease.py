"""Exercise every package writer through an installed runtime and a script host."""
import fcntl
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
    'src/install.rs::remove_all':['tests/rex_installer.py --remove'],
    'src/install/command.rs::remove_shell_command':['tests/rex_installer.py --remove'],
    'src/rex_ui/install.rs::apply':['tests/rex_installer.py install/remove'],
    'src/rex_ui/install.rs::install_packages':['tests/rex_installer.py install','activate handoff'],
    'src/rex_ui/install.rs::save':['tests/rex_installer.py install','activate handoff'],
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
    'src/rex_ui.rs::refresh':['activate','name','event','hook'],
}
def balanced(text,start):
    """The end of the bracketed group that opens at text[start]."""
    pairs={'(':')','{':'}','<':'>'};opening=text[start];closing=pairs[opening];depth,end=1,start+1
    while depth and end<len(text):
        depth+={opening:1,closing:-1}.get(text[end],0);end+=1
    return end
sources={str(path.relative_to(repo)):re.sub(r'(?ms)^#\[cfg\(test\)\]\nmod \w+ \{$.*?^\}$','',path.read_text()) for path in (repo/'src').rglob('*.rs')}
# A type that holds a lease, in a field or a variant, carries it as well.
holders={'PackageLease'}
while True:
    grown={match.group(1) for text in sources.values() for match in re.finditer(r'\b(?:struct|enum)\s+(\w+)[^;{(]*[{(]',text)
           if re.search(r'\b(?:%s)\b'%'|'.join(holders),text[match.end()-1:balanced(text,match.end()-1)])}-holders
    if not grown:break
    holders|=grown
named=re.compile(r'\b(?:%s)\b'%'|'.join(holders))
found=set()
for name,text in sources.items():
    methods=set()
    for match in re.finditer(r'\bimpl\b(?:\s*<[^{}]*?>)?\s+(\w+)[^{;]*?(?:\bfor\s+(\w+)[^{;]*)?\{',text):
        if (match.group(2) or match.group(1)) in holders-{'PackageLease'}:methods.add((match.end()-1,balanced(text,match.end()-1)))
    for match in re.finditer(r"\bfn\s+(\w+)\s*(?:<[^{}]*?>)?\s*\(",text,re.S):
        end=balanced(text,match.end()-1)
        tail=text[end:min([i for i in (text.find('{',end),text.find(';',end)) if i>=0]+[len(text)])]
        bounds=tail[tail.find('where'):] if re.search(r'\bwhere\b',tail) else ''
        held=any(start<match.start()<stop for start,stop in methods) and re.search(r'\bself\b',text[match.end():end])
        if named.search(text[match.start():end]+bounds) or held:
            found.add(name+'::'+match.group(1))
assert found==set(WRITERS),{'unmapped':sorted(found-set(WRITERS)),'stale':sorted(set(WRITERS)-found)}

def snapshot(home):
    targets=['.agents/skills/namedrop','.claude/skills/namedrop','.claude/settings.json','.codex/rules','.codex/config.toml','.local/bin/namedrop','.config/namedrop/state-directory','.config/namedrop/rex.json','.config/rex/namedrop.lua','.profile','.bashrc']
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
        if owner:(runtime/'owner').write_text(owner)
        launcher=runtime/'namedrop'
        launcher.write_text('#!/usr/bin/python3 -IB\nimport json,os,sys\nwith open('+repr(str(root/'pids'))+',"a") as f:f.write(json.dumps({"pid":os.getpid(),"args":sys.argv})+"\\n")\nos.execv('+repr(str(runtime/'current/namedrop'))+',sys.argv)\n')
        launcher.chmod(0o755)
        return state
    def rex_fixture(home):
        module=home/'.config/rex/namedrop.lua';module.parent.mkdir(parents=True,exist_ok=True)
        module.write_text('-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.\n-- stale module\n')
        record=home/'.config/namedrop/rex.json';record.parent.mkdir(parents=True,exist_ok=True)
        record.write_text(json.dumps({'module':str(module),'state':'/old','launcher':'/old/runtime/namedrop','line':'removed','predecessors':[{'state':'/retained'}]}))
        return module,record
    old=installed('herdr','herdr')
    state=installed('standalone','standalone')
    inactive=installed('inactive','standalone')
    def env(home,selected,pane='p1'):
        home.mkdir(exist_ok=True)
        return {'HOME':str(home),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','ROOT':str(root),'CHECKOUT':str(root/'repo'),'SHELLPID':str(os.getpid()),'SOCK':address,'PYTHONDONTWRITEBYTECODE':'1','HERDR_BIN_PATH':str(repo/'tests/differential/herdr'),'HERDR_SOCKET_PATH':address,'HERDR_WORKSPACE_ID':'w1','HERDR_PANE_ID':pane,'HERDR_PLUGIN_STATE_DIR':str(selected)}
    def run(home,selected,args,extra=None,launcher=None):
        e=env(home,selected,'p3' if args[0]=='name' else 'p1');e.update(extra or {})
        started=time.monotonic()
        p=subprocess.run([str((launcher or selected)/'runtime/namedrop'),*args],input='',cwd=root/'repo',env=e,capture_output=True,text=True,timeout=30)
        p.elapsed=time.monotonic()-started
        return p
    activate=['--json','activate','--input-json','{"confirmed":true}']
    p=run(root/'prewarm',state,activate);assert p.returncode==0,(p.stdout,p.stderr)
    # Deactivate runs last: it deactivates the space even when its removals are refused.
    cases={
        'activate':activate,
        'name':['name','lease-name','--type','terminal'],
        'event':['event','agent-status-changed','--input-json',json.dumps({'type':'pane_agent_status_changed','pane_id':'p1','workspace_id':'w1','agent_status':'idle'})],
        'hook':['hook','agent-status-changed'],
        'deactivate':['--json','deactivate'],
    }
    extra={'HERDR_PLUGIN_EVENT':'pane.agent_status_changed','HERDR_PLUGIN_EVENT_JSON':json.dumps({'event':'pane_agent_status_changed','data':{'type':'pane_agent_status_changed','pane_id':'p1','workspace_id':'w1','agent_status':'idle'}})}
    try:
        home=root/'home';p=run(home,old,activate);assert p.returncode==0,(p.stdout,p.stderr)
        rex_fixture(home)
        for name,args in cases.items():
            before=snapshot(home);p=run(home,inactive if name=='name' else state,args,extra)
            assert 'ND-INSTALL-002' in p.stdout+p.stderr,(name,p.returncode,p.stdout,p.stderr)
            # A refused refresh is the event's report: the event and the hook still exit 0.
            if name in ['event','hook','deactivate']:assert p.returncode==0,(name,p.returncode,p.stdout,p.stderr)
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
            module,rex_record=rex_fixture(own)
            before=snapshot(own);time.sleep(.6)
            p=run(own,inactive if name=='name' else state,args,extra)
            assert 'ND-INSTALL-002' not in p.stdout+p.stderr,(name,p.stdout,p.stderr)
            assert p.returncode==0,(name,p.stdout,p.stderr)
            if name=='hook':
                # The worker stages files beside SKILL.md before it renames one over it.
                until=time.monotonic()+10
                while skill.read_text()!=expected_skill and time.monotonic()<until:time.sleep(.05)
            if name!='deactivate':
                until=time.monotonic()+10
                while json.loads(rex_record.read_text())['state']!=str(selected) and time.monotonic()<until:time.sleep(.05)
                registered=json.loads(rex_record.read_text())
                assert registered['state']==str(selected),(name,registered)
                # The launcher is the running executable's resolved path; the state is as given.
                assert Path(registered['launcher'])==(selected/'runtime/namedrop').resolve(),(name,registered)
                assert registered['line']=='removed' and registered['predecessors']==[{'state':'/retained'}],(name,registered)
                assert 'namedrop_about' in module.read_text() and '-- stale module' not in module.read_text(),name
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
        def settled(home,before,seconds=3):
            """Whether the files stay as they were while a hook's worker runs."""
            until=time.monotonic()+seconds
            while snapshot(home)==before and time.monotonic()<until:time.sleep(.05)
            return snapshot(home)==before
        def ok(p,*context):
            assert p.returncode==0 and 'ND-INSTALL-002' not in p.stdout+p.stderr,(context,p.returncode,p.stdout,p.stderr)
            return p

        # Two Herdr states in one HOME: the second takes the record and the packages, as before the lease.
        two=root/'two-herdr';legacy=installed('herdr-legacy',None)
        ok(run(two,old,activate),'first herdr');ok(run(two,legacy,activate),'second herdr')
        assert (two/'.config/namedrop/state-directory').read_text()==str(legacy)+'\n'
        for package in ['.agents/skills/namedrop','.claude/skills/namedrop']:
            assert str(legacy) in (two/package/'scripts/session-start').read_text(),package
        # A per-state lock that cannot be taken fails each removal, and the space is still deactivated.
        lock=legacy/'runtime/packages.lock';lock.unlink(missing_ok=True);lock.symlink_to('/dev/null')
        p=ok(run(two,legacy,['--json','deactivate']),'deactivate with a broken lock');lock.unlink()
        data=json.loads(p.stdout)['data']
        assert data['deactivated'] is True,data
        for removal in ['claude_permissions','codex_sandbox','subagent_hooks']:
            assert data[removal]['error']=='runtime package lock is not a regular file',(removal,data)
        # Herdr's runtime still serves another state directory.
        other=root/'other-state';other.mkdir()
        ok(run(root/'other-home',other,activate,launcher=old),'herdr runtime, other state')
        print('two herdr states share the slot',flush=True)

        # The status hook takes no package lock and writes nothing of its own.
        fresh=root/'fresh';never=installed('herdr-never','herdr')
        ok(run(fresh,never,cases['hook'],extra),'never activated hook');time.sleep(1.5)
        assert not (fresh/'.config/namedrop').exists(),sorted(p.name for p in (fresh/'.config/namedrop').iterdir())
        held=open(home/'.config/namedrop/packages.lock','a+');fcntl.flock(held,fcntl.LOCK_EX)
        try:
            p=ok(run(home,old,cases['hook'],extra),'hook under a held lock')
            assert p.elapsed<2,p.elapsed
            p=ok(run(home,old,cases['event']),'event under a held lock')
            assert '"drain"' in p.stdout and p.elapsed<10,(p.elapsed,p.stdout)
        finally:
            held.close()
        print('the status hook neither waits nor claims',flush=True)

        mixed=root/'herdr-over-standalone'
        ok(run(mixed,state,activate),'standalone first')
        # Herdr's other writers leave the packages to the next activation and still do their own work.
        herdr_inactive=installed('herdr-inactive','herdr')
        for name in ['event','hook','name','deactivate']:
            before=snapshot(mixed)
            p=ok(run(mixed,herdr_inactive if name=='name' else old,cases[name],extra),'herdr writer over standalone',name)
            if name=='event':assert '"drain"' in p.stdout,p.stdout
            if name=='deactivate':assert json.loads(p.stdout)['data']['deactivated'] is True,p.stdout
            assert settled(mixed,before) if name=='hook' else snapshot(mixed)==before,('herdr writer over standalone changed files',name)
            print('herdr',name,'over a standalone record unchanged',flush=True)
        # The takeover replaces U3's temporary refusal test in this commit.
        p=ok(run(mixed,old,activate),'Herdr takes over')
        data=json.loads(p.stdout)['data'];assert data['handoff'][0]['notice'].find('were not carried over')>=0,data
        assert (mixed/'.config/namedrop/state-directory').read_text()==str(old)+'\n'
        before=snapshot(mixed);p=run(mixed,state,activate)
        assert p.returncode==69 and 'ND-INSTALL-001' in p.stderr and str(old) in p.stderr,(p.returncode,p.stdout,p.stderr)
        assert snapshot(mixed)==before,'retired standalone activation changed shared files'
        print('takeover stays with Herdr',flush=True)
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
