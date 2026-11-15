"""Installer filesystem integration against a scripted local control server.

Use rex_install_live.py for the actual Lua parser and scratch Rex builds.
"""
import contextlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import threading

BINARY=Path(sys.argv[1]).resolve()
LINE='pcall(require, "namedrop");'
MANAGED='-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.'

def exact(conn,n):
    data=b''
    while len(data)<n:
        part=conn.recv(n-len(data))
        if not part:raise EOFError
        data+=part
    return data

def frame(conn):
    a,b=exact(conn,2);n=b&127
    if n==126:n=struct.unpack('!H',exact(conn,2))[0]
    if n==127:n=struct.unpack('!Q',exact(conn,8))[0]
    mask=exact(conn,4) if b&128 else b''
    data=exact(conn,n)
    if mask:data=bytes(c^mask[i%4] for i,c in enumerate(data))
    if a&15==8:raise EOFError
    return json.loads(data)

def send(conn,value):
    data=json.dumps(value).encode();n=len(data)
    conn.sendall(bytes([0x81,n]) + data if n<126 else bytes([0x81,126])+struct.pack('!H',n)+data)

class Server:
    def __init__(self,root,config):
        self.root=root;self.config=config;self.socket=root/'r.sock';self.calls=[];self.reloads=0;self.fail_at=None;self.warning=False;self.wrong=None;self.edit_during_error=False;self.fail_before=False;self.check_lock=True
        self.listener=socket.socket(socket.AF_UNIX);self.listener.bind(str(self.socket));self.listener.listen();self.done=False
        self.thread=threading.Thread(target=self.serve);self.thread.start()
    def close(self):
        self.done=True
        with contextlib.suppress(OSError):
            c=socket.socket(socket.AF_UNIX);c.connect(str(self.socket));c.close()
        self.thread.join(3);self.listener.close();assert not self.thread.is_alive()
    def serve(self):
        while not self.done:
            try:conn,_=self.listener.accept()
            except OSError:return
            with conn:
                conn.settimeout(3)
                try:
                    head=b''
                    while not head.endswith(b'\r\n\r\n'):head+=exact(conn,1)
                    conn.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n')
                    assert frame(conn)['type']=='hello'
                    send(conn,{'type':'welcome','protocol':'rex.control.v1','api_revision':14,'limits':{'max_message_bytes':4194304}})
                    while True:
                        request=frame(conn);method=request['method'];self.calls.append(method)
                        if method=='server.status':value={'pid':os.getpid(),'listeners':[{'primary':True,'transport':'unix','addresses':[str(self.socket)]}]}
                        elif method=='client.keymap':value=self.keymap()
                        elif method=='server.config_reload':
                            self.reloads+=1
                            if self.check_lock:
                                import fcntl
                                lock=self.root/'home/.config/namedrop/packages.lock'
                                if lock.exists():
                                    with lock.open('r+') as f:fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)
                            failed=self.fail_before or self.reloads==self.fail_at
                            if failed and self.edit_during_error:self.config.write_text('-- edited by person\n')
                            value={'generation':self.reloads,'diagnostics':[{'severity':'error' if failed else 'warning','message':'fixture diagnostic'}] if failed or self.warning else []}
                        elif method=='client.action':value={'result':self.about(),'client_actions':[]}
                        else:raise AssertionError(method)
                        send(conn,{'type':'response','id':request['id'],'seq':1,'payload':value})
                except (EOFError,OSError):pass
    def keymap(self):
        names=['activate','name','manage','doctor','status','tour','channels','pause','resume','deactivate','about']
        loaded=self.config.exists() and LINE in self.config.read_text().splitlines()
        return {'config':{'path':str(self.config),'exists':self.config.exists()},'actions':[{'name':'namedrop_'+n} for n in names] if loaded else [],'bindings':[],'diagnostics':[]}
    def about(self):
        if not self.config.exists() or LINE not in self.config.read_text().splitlines():return None
        record=json.loads((self.root/'home/.config/namedrop/rex.json').read_text())
        result={k:record[k] for k in ['module','launcher','state']};result['module']='@'+result['module']
        if self.wrong:result[self.wrong]='/another/install'
        return result

with tempfile.TemporaryDirectory(prefix='nd-u7-protocol-',dir='/tmp') as folder:
    root=Path(folder)
    def case(name,contents=b'-- personal config\n',custom=False):
        place=root/name;place.mkdir();(place/'home').mkdir();(place/'state/runtime').mkdir(parents=True);(place/'state/runtime/owner').write_text('standalone\n')
        config=place/('custom/init.lua' if custom else 'home/.config/rex/init.lua');config.parent.mkdir(parents=True,exist_ok=True)
        if contents is not None:config.write_bytes(contents)
        return place,config,Server(place,config)
    def run(place,server,*args,ok=True):
        env={'HOME':str(place/'home'),'PATH':'/usr/bin:/bin','NAMEDROP_STATE_DIR':str(place/'state'),'REX_SERVER':'unix://'+str(server.socket),'SHELL':'/bin/none'}
        p=subprocess.run([str(BINARY),'--json','install',*args,'--input-json','{"confirmed":true}'],env=env,input=b'',capture_output=True,timeout=30)
        if ok:assert p.returncode==0,(args,p.returncode,p.stdout,p.stderr)
        else:assert p.returncode!=0,(args,p.stdout,p.stderr)
        return p
    def files(place):return {str(p.relative_to(place)):p.read_bytes() for p in place.rglob('*') if p.is_file()}
    for name,contents in [('absent',None),('return',b'return {}\n'),('comment',b'-- no newline'),('paren',b'(function() return 1 end)()\n'),('crlf',b'-- config\r\n'),('comment-require',b'-- require("namedrop")\n'),('string-require',b'local s = \'require("namedrop")\'\n')]:
        place,config,server=case(name,contents,custom=name=='return')
        try:
            run(place,server);after=files(place);run(place,server);assert files(place)==after,'second install changed a file'
            record=json.loads((place/'home/.config/namedrop/rex.json').read_text());assert record['line']=='inserted' and record['config']==str(config) and record['module']==str(config.parent/'namedrop.lua')
            run(place,server,'--remove');assert config.read_bytes()==(contents or b'');assert not (config.parent/'namedrop.lua').exists();assert not (place/'home/.config/namedrop/rex.json').exists()
            assert not (place/'home/.agents/skills/namedrop').exists();assert (place/'state/runtime/retired.json').exists()
        finally:server.close()
    place,config,server=case('ownership')
    try:
        (config.parent/'namedrop.lua').write_text('-- my module\n');p=run(place,server,ok=False);assert b'not managed' in p.stdout;assert config.read_bytes()==b'-- personal config\n'
    finally:server.close()
    place,config,server=case('printed')
    try:
        original=config.read_bytes();run(place,server,'--print-config');assert config.read_bytes()==original
        path=place/'home/.config/namedrop/rex.json';r=json.loads(path.read_text());assert r['line']=='printed'
        config.write_text(LINE+'\n'+original.decode());run(place,server);assert json.loads(path.read_text())['line']=='present'
        run(place,server,'--remove');assert config.read_text().startswith(LINE),'personal line was removed'
    finally:server.close()
    place,config,server=case('removed-line')
    try:
        run(place,server);config.write_text('-- person removed it\n');run(place,server);assert config.read_text()=='-- person removed it\n';assert json.loads((place/'home/.config/namedrop/rex.json').read_text())['line']=='removed'
        run(place,server);assert config.read_text()=='-- person removed it\n'
    finally:server.close()
    place,config,server=case('symlink')
    try:
        target=place/'real.lua';config.rename(target);config.symlink_to(target);run(place,server);assert config.is_symlink() and target.read_text()=='-- personal config\n';assert (config.parent/'namedrop.lua').is_file()
        config.unlink();config.write_text('-- now regular\n');run(place,server);assert config.read_text().startswith(LINE)
    finally:server.close()
    for scenario in ['before','after','warning','person-change']:
        place,config,server=case(scenario)
        try:
            before=config.read_bytes();server.fail_before=scenario=='before';server.fail_at=2 if scenario in ['after','person-change'] else None;server.warning=scenario=='warning';server.edit_during_error=scenario=='person-change'
            p=run(place,server,ok=scenario=='warning')
            if scenario in ['before','after']:assert config.read_bytes()==before
            if scenario=='person-change':assert config.read_text()=='-- edited by person\n' and list(config.parent.glob('*.bak'))
        finally:server.close()
    for field in ['module','launcher','state']:
        place,config,server=case('wrong-'+field)
        try:
            server.wrong=field;p=run(place,server,ok=False);assert b'not confirmed' in p.stdout and b'/another/install' in p.stdout;assert config.read_text().startswith(LINE)
        finally:server.close()
print('protocol installer guards, rollback, identity, idempotence and removal passed')
