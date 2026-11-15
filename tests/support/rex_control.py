"""A local control-protocol fixture. It does not run Rex or load Lua."""
import contextlib
import json
import os
import socket
import struct
import threading

LINE='pcall(require, "namedrop");'

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
                        elif method=='session.list':value={'sessions':[]}
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
