"""Read and safely cancel only an owned testbed capture profile."""
import json
from pathlib import Path
import sys

sys.dont_write_bytecode=True
root=Path(__file__).resolve().parents[4]
sys.path.insert(0,str(root/'testbed/lib'))
import testbed

name,operation,*extra=sys.argv[1:]
if not name.startswith('test-'):raise ValueError('capture requires a test- profile')
p=testbed.Profile(name);p.owned()
assert p.socket.name.startswith('nd-tb-')
assert p.env['HOME']!=str(Path.home()) and p.env['XDG_CONFIG_HOME']!=str(Path.home()/'.config')
if operation=='info':
 revision='8ea5c1a21d61da0e2539a2d8cae0638557c83dab'
 product=p.run(['git','diff','--quiet',revision,'HEAD','--','src','experimental/channels/src','Cargo.toml','Cargo.lock','herdr-plugin.toml'],cwd=p.state['plugin_root'],check=False)
 addresses=json.loads(p.namedrop('address','list','--json').stdout)['data']
 print(json.dumps({'workspace':p.state['workspace'],'panes':p.state['panes'],'profileHome':p.env['HOME'],'profileRoot':str(p.dir),'source':p.state['source'],'version':p.namedrop_version(),'productMatches':product.returncode==0,'addresses':addresses}))
elif operation=='open':
 result=p.namedrop('--herdr-pane',p.state['panes']['shell'],'channels','--no-open','--json')
 if result.returncode:raise RuntimeError(result.stdout)
 print(result.stdout)
elif operation=='pane':
 alias=extra[0]
 if alias not in ('claude','codex'):raise ValueError('agent capture only')
 print(p.run(['herdr','pane','read',p.state['panes'][alias],'--source','visible','--ansi']).stdout,end='')
elif operation in ('cancel','clear'):
 alias=extra[0]
 if alias not in ('claude','codex'):raise ValueError('demo agents only')
 if operation=='cancel' and p.herdr('pane','get',p.state['panes'][alias])['pane'].get('agent_status')!='blocked':
  raise ValueError('cancel requires a real blocked agent')
 key='esc' if operation=='cancel' else 'ctrl+l'
 print(p.run(['herdr','pane','send-keys',p.state['panes'][alias],key]).stdout,end='')
else:raise ValueError('unknown capture operation')
