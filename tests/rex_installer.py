"""Installer filesystem integration against a scripted local control server.

Use rex_install_live.py for the actual Lua parser and scratch Rex builds.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

BINARY=Path(sys.argv[1]).resolve()
LINE='pcall(require, "namedrop");'
MANAGED='-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.'

sys.path.insert(0, str(Path(__file__).resolve().parent))
from support.rex_control import Server

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
    def tree(place):
        return {str(p.relative_to(place)):(os.readlink(p) if p.is_symlink() else p.read_bytes() if p.is_file() else None)
                for p in place.rglob('*') if p.name!='r.sock'}
    # install.sh lists the preview's targets before it asks, and the report's
    # after. Every file the install writes must be under one of them.
    for shell,login in [('/bin/zsh',None),('/bin/bash','.bash_profile'),('/bin/bash',None),('/bin/sh',None)]:
        place,config,server=case('targets'+shell.replace('/','-')+str(login))
        try:
            if login:(place/'home'/login).write_text('# mine\n')
            extra={'SHELL':shell,'CLAUDE_CONFIG_DIR':str(place/'claude config'),'CODEX_HOME':str(place/'codex home')}
            env={'HOME':str(place/'home'),'PATH':'/usr/bin:/bin','NAMEDROP_STATE_DIR':str(place/'state'),'REX_SERVER':'unix://'+str(server.socket),**extra}
            before=tree(place)
            p=subprocess.run([str(BINARY),'--install-preview'],env=env,input=b'',capture_output=True,timeout=30)
            assert p.returncode==0,(shell,p)
            assert tree(place)==before,'the preview wrote a file'
            preview=json.loads(p.stdout);listed=[(t['what'],t['path']) for t in preview['targets']]
            assert preview['config']==str(config) and preview['module']==str(config.parent/'namedrop.lua'),preview
            p=subprocess.run([str(BINARY),'--json','install','--input-json','{"confirmed":true}'],env=env,input=b'',capture_output=True,timeout=30)
            assert p.returncode==0,(shell,p)
            report=json.loads(p.stdout)['data']
            assert [(t['what'],t['path']) for t in report['targets']]==listed,'the report names other targets'
            after=tree(place);paths=[Path(path) for _,path in listed]+[place/'state']
            for name in sorted(k for k in after.keys()|before.keys() if before.get(k)!=after.get(k)):
                path=place/name
                backup=path.parent==config.parent and path.name.startswith(config.name+'.namedrop-')
                assert backup or any(path==t or t in path.parents for t in paths),(shell,name,'written but not listed')
            for what,path in listed:assert Path(path).exists(),(shell,what,path,'listed but not written')
            startup={path for what,path in listed if what=='PATH lines'}
            home=place/'home'
            expected={'/bin/zsh':{home/'.zshrc'},'/bin/bash':{home/'.bashrc',home/(login or '.profile')},'/bin/sh':set()}[shell]
            assert startup=={str(p) for p in expected},(shell,startup)
            assert {str(p) for p in report['shell_command']['shell_startup_files']}==startup
            assert ('Claude Code package',str(home/'.claude/skills/namedrop')) in listed
            assert ('Codex package',str(home/'.agents/skills/namedrop')) in listed
            assert ('Claude Code permissions',str(place/'claude config/settings.json')) in listed
        finally:server.close()
    # Without a server the preview stops in plain words and writes nothing.
    place,config,server=case('no-server');server.close()
    env={'HOME':str(place/'home'),'PATH':'/usr/bin:/bin','REX_SERVER':'unix://'+str(place/'gone.sock'),'SHELL':'/bin/zsh'}
    before=tree(place)
    p=subprocess.run([str(BINARY),'--install-preview'],env=env,input=b'',capture_output=True,timeout=30)
    assert p.returncode==69 and not p.stdout,p
    message=p.stderr.decode()
    assert 'Start Rex' in message and 'sh scripts/install.sh again' in message and '{' not in message,message
    assert tree(place)==before
print('protocol installer guards, rollback, identity, idempotence, removal and targets passed')
