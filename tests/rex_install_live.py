"""U7 installer Lua/config checks on disposable Rex servers.

Usage: python3 -I tests/rex_install_live.py BINARY REX_BINARY OUT
Every Rex call names the scratch socket and disables autostart. The launcher
records every self-spawn. Cleanup signals only recorded, revalidated PIDs.
"""
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

binary,rex,out=map(lambda p:Path(p).resolve(),sys.argv[1:4])
repo=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('runtime_cleanup',repo/'scripts/runtime.py')
runtime=importlib.util.module_from_spec(spec);sys.path.insert(0,str(repo/'scripts'));spec.loader.exec_module(runtime)
out.mkdir(parents=True,exist_ok=True)
LINE='pcall(require, "namedrop");'

class Fixture:
    def __init__(self,name,contents,custom=False,different_home=False):
        self.root=out/name
        if self.root.exists():raise RuntimeError('Use a fresh OUT directory: '+str(self.root))
        self.root.mkdir();self.home=self.root/'home';self.home.mkdir();self.server_home=self.root/'server-home' if different_home else self.home;self.server_home.mkdir(exist_ok=True)
        self.config=self.root/'custom/config.lua' if custom else self.server_home/'.config/rex/init.lua';self.config.parent.mkdir(parents=True)
        if contents is not None:self.config.write_bytes(contents)
        self.state=self.root/'standalone';release=self.state/'runtime/releases'/hashlib.sha256(binary.read_bytes()).hexdigest();release.mkdir(parents=True)
        shutil.copy2(binary,release/'namedrop');shutil.copy2(repo/'runtime.json',release/'runtime.json');(self.state/'runtime/current').symlink_to('releases/'+release.name);(self.state/'runtime/owner').write_text('standalone\n')
        self.launcher=self.state/'runtime/namedrop';self.launcher.write_text('#!'+sys.executable+' -IB\nimport json,os,sys\nwith open('+repr(str(self.root/'pids.jsonl'))+',"a") as f:f.write(json.dumps({"pid":os.getpid(),"argv":sys.argv})+"\\n")\nos.execv('+repr(str(self.state/'runtime/current/namedrop'))+',sys.argv)\n');self.launcher.chmod(0o700)
        # Short socket paths fit sockaddr_un even when OUT is a long scratch path.
        self.socket=Path('/tmp')/('nd-u7-'+str(os.getpid())+'-'+name+'.sock')
        self.env={'HOME':str(self.home),'PATH':'/usr/bin:/bin','SHELL':'/bin/none','REX_SERVER':'unix://'+str(self.socket),'NAMEDROP_STATE_DIR':str(self.state)}
        server_env=dict(self.env,HOME=str(self.server_home))
        args=[str(rex),'--autostart=false','-S','unix://'+str(self.socket),'server','run','--listen','unix://'+str(self.socket),'--state-dir',str(self.root/'rex-state'),'--restore=false']
        if custom:args+=['--config',str(self.config)]
        self.log=(self.root/'rex.log').open('w');self.server=subprocess.Popen(args,env=server_env,stdout=self.log,stderr=subprocess.STDOUT)
        (self.root/'server.pid').write_text(str(self.server.pid)+'\n')
        deadline=time.monotonic()+10
        while not self.socket.exists():
            if self.server.poll() is not None or time.monotonic()>deadline:raise RuntimeError('Scratch Rex failed: '+str(self.root/'rex.log'))
            time.sleep(.05)
    def rx(self,*args,ok=True):
        p=subprocess.run([str(rex),'--autostart=false','-S','unix://'+str(self.socket),*args],env=self.env,capture_output=True,text=True,timeout=30)
        if ok:assert p.returncode==0,(args,p.stdout,p.stderr)
        return p
    def nd(self,*args,ok=True):
        p=subprocess.run([str(self.launcher),'--json',*args],env=self.env,input='',capture_output=True,text=True,timeout=75)
        with (self.root/'commands.jsonl').open('a') as f:f.write(json.dumps({'args':args,'code':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
        if ok:assert p.returncode==0,(args,p.returncode,p.stdout,p.stderr)
        else:assert p.returncode!=0,(args,p.stdout,p.stderr)
        return p
    def install(self,*args,ok=True):return self.nd('install',*args,'--input-json','{"confirmed":true}',ok=ok)
    def close(self):
        self.server.terminate()
        try:self.server.wait(5)
        except subprocess.TimeoutExpired:self.server.kill();self.server.wait()
        self.log.close()
        pids=self.root/'pids.jsonl'
        for line in pids.read_text().splitlines() if pids.exists() else []:
            pid=json.loads(line)['pid'];found=runtime.process(pid)
            if not found or not found[0].startswith(str(self.state/'runtime/releases')+'/'):continue
            if runtime.helper_kind(found[1],found[2],self.state) or any(word in found[1] for word in ['bridge','worker','agent-status-worker','dashboard-server']):
                with contextlib.suppress(ProcessLookupError):os.kill(pid,signal.SIGTERM)
        self.socket.unlink(missing_ok=True)

def snapshot(f):
    paths=[p for p in f.home.rglob('*') if p.is_file()]
    paths+=[p for p in f.config.parent.glob('*') if p.is_file()]
    return {str(p):p.read_bytes() for p in paths}

shapes=[('absent',None),('return',b'return {}\n'),('comment',b'-- no newline'),('runtime',b'error("mine broke")\n'),('syntax',b'local = 1\n'),('shebang',b'#!/usr/bin/env lua\n'),('bom',b'\xef\xbb\xbf-- config\n'),('paren',b'(function() return 1 end)()\n'),('binding',b'rex.action{name="mine",run=function()end}\nrex.bind("alt+shift+cmd+i","mine")\n'),('crlf',b'-- config\r\n')]
for name,contents in shapes:
    broken=name in {'runtime','syntax','shebang','bom'}
    # Rex may reject a broken file at startup, so break it only after startup.
    f=Fixture(name,b'-- initial\n' if broken else contents,custom=name=='return',different_home=name=='binding')
    try:
        if broken:f.config.write_bytes(contents)
        f.install(ok=not broken)
        if broken:assert f.config.read_bytes()==contents;continue
        record=json.loads((f.home/'.config/namedrop/rex.json').read_text());assert record['config']==str(f.config) and record['module']==str(f.config.parent/'namedrop.lua')
        about=json.loads(f.rx('do','namedrop_about','--json').stdout);about=about.get('result',about);assert about['launcher']==str(f.launcher) and about['state']==str(f.state) and about['module'].removeprefix('@')==record['module']
        actions=json.loads(f.rx('actions','--json').stdout);actions=actions.get('actions',[]) if isinstance(actions,dict) else actions
        assert len([a for a in actions if a['name'].startswith('namedrop_')])==11
        before=snapshot(f);f.install();assert snapshot(f)==before,'second install changed a file'
        cached=f.root/'cached-codex';shutil.copytree(f.home/'.agents/skills/namedrop',cached)
        f.install('--remove');assert f.config.read_bytes()==(contents or b'')
        before={str(p.relative_to(f.state)):p.read_bytes() for p in f.state.rglob('*') if p.is_file()}
        for script in ['session-start','user-prompt-submit','stop','subagent']:
            env=dict(f.env,NAMEDROP_HOOK_RUNTIME='codex');p=subprocess.run(['/bin/sh',str(cached/'scripts'/script)],env=env,input='{}',capture_output=True,text=True,timeout=5)
            assert p.returncode==0 and not p.stdout and not p.stderr,(script,p)
        assert {str(p.relative_to(f.state)):p.read_bytes() for p in f.state.rglob('*') if p.is_file()}==before
        print(name,'passed',flush=True)
    finally:f.close()
print('U7 real Rex install/config/readback/remove/cached-hook checks passed')

# The Herdr differential's script supplies Herdr replies; Rex stays a real scratch server.
import fcntl
import socket
import threading

def wait(read,accepts,description):
    until=time.monotonic()+15
    while time.monotonic()<until:
        value=read()
        if accepts(value):return value
        time.sleep(.05)
    raise AssertionError(description+': '+repr(value))

def handoff_fixture(name,stopped=None):
    f=Fixture(name,b'-- config\n');f.install()
    successor=f.root/'herdr';shutil.copytree(f.state/'runtime',successor/'runtime',symlinks=True)
    (successor/'runtime/owner').write_text('herdr\n')
    launcher=successor/'runtime/namedrop';launcher.write_text(f.launcher.read_text().replace(str(f.state),str(successor)));launcher.chmod(0o700)
    checkout=f.root/'repo';checkout.mkdir()
    (f.root/'repo2').mkdir()
    herdr_socket=Path('/tmp')/('nd-u7-h-'+str(os.getpid())+'-'+name+'.sock')
    listener=socket.socket(socket.AF_UNIX);listener.bind(str(herdr_socket));listener.listen();done=threading.Event()
    def accept():
        while not done.is_set():
            try:c,_=listener.accept();c.close()
            except OSError:return
    thread=threading.Thread(target=accept,daemon=True);thread.start()
    env={'HOME':str(f.home),'PATH':'/usr/bin:/bin','ROOT':str(f.root),'CHECKOUT':str(checkout),'SHELLPID':str(os.getpid()),'SOCK':str(herdr_socket),'HERDR_BIN_PATH':str(repo/'tests/differential/herdr'),'HERDR_PLUGIN_STATE_DIR':str(successor),'HERDR_SOCKET_PATH':str(herdr_socket),'HERDR_WORKSPACE_ID':'w1','HERDR_PANE_ID':'p1'}
    def hr(*args,ok=True,async_=False):
        command=[str(launcher),'--endpoint',str(herdr_socket),'--space','w1','--pane','p1','--json',*args]
        if async_:return subprocess.Popen(command,env=env,cwd=checkout,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        p=subprocess.run(command,env=env,cwd=checkout,input='',capture_output=True,text=True,timeout=75)
        with (f.root/'herdr-commands.jsonl').open('a') as log:log.write(json.dumps({'args':args,'code':p.returncode,'stdout':p.stdout,'stderr':p.stderr})+'\n')
        if ok:assert p.returncode==0,(args,p.returncode,p.stdout,p.stderr)
        return p
    def close():
        f.close();done.set();listener.close();herdr_socket.unlink(missing_ok=True)
        # Same scratch root, both retained runtime trees, and exact executable/argv.
        for line in (f.root/'pids.jsonl').read_text().splitlines():
            pid=json.loads(line)['pid'];found=runtime.process(pid)
            if found and found[0].startswith(str(successor/'runtime/releases')+'/'):
                with contextlib.suppress(ProcessLookupError):os.kill(pid,signal.SIGTERM)
    return f,successor,hr,close

AGENT='''import json,os,subprocess,sys,time
launcher,state,ended=sys.argv[1:]
env=dict(os.environ,NAMEDROP_HOOK_RUNTIME="codex")
p=subprocess.run([launcher,"hook","session-start","--state-directory",state],input=json.dumps({"session_id":"u7-"+str(os.getpid())}),env=env,capture_output=True,text=True)
if p.returncode:raise SystemExit(p.stderr)
while not os.path.exists(ended):time.sleep(.05)
'''

def bind_agents(f,count=2):
    script=f.root/'standin.py';script.write_text(AGENT)
    blocks=[];sessions=[]
    for i in range(count):
        created=json.loads(f.rx('new','u7-agent-'+str(i),'--shell','none','--json','--',sys.executable,'-I',str(script),str(f.launcher),str(f.state),str(f.root/('end-'+str(i)))).stdout)
        sessions.append(created['session_id']);blocks.append(created['initial_windows'][0]['block_ids'][0])
    wait(lambda:list((f.state/'rex/sessions').glob('*.json')) if (f.state/'rex/sessions').exists() else [],lambda entries:len(entries)==count,'stand-in SessionStart bindings')
    return sessions,blocks

def registration(f):return json.loads((f.home/'.config/namedrop/rex.json').read_text())
def warnings(hr):
    report=hr('doctor',ok=False)
    return [i for i in json.loads(report.stdout)['data']['issues'] if i['code']=='ND-INSTALL-001']

def shared(f):
    roots=[f.home/'.agents/skills/namedrop',f.home/'.claude/skills/namedrop',f.home/'.claude/settings.json',f.home/'.codex',f.home/'.local/bin/namedrop',f.home/'.config/namedrop/state-directory']
    return {str(p):p.read_bytes() for root in roots for p in (root.rglob('*') if root.is_dir() else [root]) if p.is_file()}

for scenario in ['takeover','stopped-before-marker','stopped-after-marker','writer-after-marker','partial-package-write','rex-half-removed','server-stopped','line-printed','line-removed']:
    f,successor,hr,close=handoff_fixture(scenario)
    try:
        sessions,blocks=bind_agents(f)
        cache=f.root/'cached-codex';shutil.copytree(f.home/'.agents/skills/namedrop',cache)
        old_bindings=[{k:v[k] for k in ['endpoint','block','agent']} for p in (f.state/'rex/sessions').glob('*.json') for v in [json.loads(p.read_text())]]
        record=registration(f)
        if scenario.startswith('stopped-') or scenario in {'writer-after-marker','partial-package-write','rex-half-removed'}:
            record['predecessors']=[{'state':str(f.state),'sealed':False,'agents':old_bindings}]
            (f.home/'.config/namedrop/rex.json').write_text(json.dumps(record))
            if scenario=='stopped-before-marker':
                before=shared(f);assert not warnings(hr);assert shared(f)==before
            else:(f.state/'runtime/retired.json').write_text(json.dumps({'reason':'herdr_handoff','successor':str(successor),'at':'2026-11-15T00:00:00Z'}))
        if scenario=='writer-after-marker':
            hr('event','agent-status-changed','--input-json',json.dumps({'type':'pane_agent_status_changed','pane_id':'p1','workspace_id':'w1','agent_status':'idle'}),ok=False)
            assert warnings(hr)
        if scenario=='partial-package-write':
            (f.home/'.config/namedrop/state-directory').write_text(str(successor)+'\n')
            (f.home/'.agents/skills/namedrop/SKILL.md').write_text('partial old package\n')
        if scenario=='rex-half-removed':hr('install','--remove','--input-json','{"confirmed":true}')
        if scenario=='server-stopped':f.server.terminate();f.server.wait(5)
        if scenario in {'line-printed','line-removed'}:
            record['line']=scenario.removeprefix('line-');(f.home/'.config/namedrop/rex.json').write_text(json.dumps(record))
        report=json.loads(hr('activate','--input-json','{"confirmed":true}').stdout)['data']
        assert report['handoff'] and 'were not carried over' in report['handoff'][0]['notice']
        assert (f.state/'runtime/retired.json').exists()
        assert (f.home/'.config/namedrop/state-directory').read_text()==str(successor)+'\n'
        if scenario=='rex-half-removed':
            assert not (f.home/'.config/namedrop/rex.json').exists() and not warnings(hr);continue
        record=registration(f);assert len(record['predecessors'])==1 and record['predecessors'][0]['sealed'] is True
        assert record['predecessors'][0]['agents']==sorted(old_bindings,key=lambda a:(a['endpoint'],a['block'],a['agent']))
        if scenario not in {'server-stopped','line-printed','line-removed'}:
            about=json.loads(f.rx('do','namedrop_about','--json').stdout);about=about.get('result',about);assert about['launcher']==str(successor/'runtime/namedrop')
        before=shared(f);p=f.nd('activate','--input-json','{"confirmed":true}',ok=False);assert p.returncode==69 and 'ND-INSTALL-001' in p.stderr and shared(f)==before
        for script in ['session-start','user-prompt-submit','stop','subagent']:
            p=subprocess.run(['/bin/sh',str(cache/'scripts'/script)],env=f.env,input='{}',capture_output=True,text=True,timeout=5);assert p.returncode==0 and not p.stdout and not p.stderr
        if scenario=='takeover':
            warn=warnings(hr);assert len(warn)==1 and '2 agents on Rex still run' in json.dumps(warn) and all(block in json.dumps(warn) for block in blocks)
            (f.root/'end-0').touch();wait(lambda:json.dumps(warnings(hr)),lambda text:'1 agent on Rex still runs' in text,'one old agent remains')
            hr('install','--remove','--input-json','{"confirmed":true}');assert not warnings(hr)
        print(scenario,'passed',flush=True)
    finally:close()
print('U7 real Rex handoff replay, takeover, cached hooks and fresh Doctor checks passed')

for operation in ['handoff','removal']:
    for timeout in [False,True]:
        f,successor,hr,close=handoff_fixture(operation+('-timeout' if timeout else '-drain'))
        try:
            gate=(f.state/'runtime/commands.lock').open('r+');fcntl.flock(gate,fcntl.LOCK_SH)
            before=shared(f);started=time.monotonic()
            if operation=='handoff':p=hr('activate','--input-json','{"confirmed":true}',async_=True)
            else:p=subprocess.Popen([str(f.launcher),'--json','install','--remove','--input-json','{"confirmed":true}'],env=f.env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            (f.root/'draining-command.pid').write_text(str(p.pid)+'\n')
            wait(lambda:runtime.gate_holders(f.state/'runtime/commands.lock'),lambda holders:any(row[0]==p.pid for row in holders),'retiring command reached the held gate')
            assert not (f.state/'runtime/retired.json').exists() and shared(f)==before
            if not timeout:gate.close()
            stdout,stderr=p.communicate(timeout=75)
            if timeout:
                assert p.returncode!=0 and time.monotonic()-started>=59 and 'Namedrop on Rex is busy. Close any Namedrop screen' in stdout+stderr
                assert not (f.state/'runtime/retired.json').exists() and shared(f)==before
                gate.close()
            else:
                assert p.returncode==0,(stdout,stderr)
                assert (f.state/'runtime/retired.json').exists()
                refused=f.nd('doctor',ok=False);assert refused.returncode==69
            print(operation,'timeout' if timeout else 'drain','passed',flush=True)
        finally:close()
