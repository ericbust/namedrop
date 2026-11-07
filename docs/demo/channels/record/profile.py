"""Read and safely drive only an owned testbed capture profile."""
import json
from pathlib import Path
import sys

sys.dont_write_bytecode=True
root=Path(__file__).resolve().parents[4]
sys.path.insert(0,str(root/'testbed/lib'))
import testbed

PRODUCT='82d381c94db2c8be888a651b5904c49cec396dd0'
PRODUCT_FILES=['src','experimental/channels/src','Cargo.toml','Cargo.lock','herdr-plugin.toml']

name,operation,*extra=sys.argv[1:]
if not name.startswith('test-'):raise ValueError('capture requires a test- profile')
p=testbed.Profile(name);p.owned()
assert p.socket.name.startswith('nd-tb-')
assert p.env['HOME']!=str(Path.home()) and p.env['XDG_CONFIG_HOME']!=str(Path.home()/'.config')

def agent_pane(alias):
 if alias not in ('claude','codex'):raise ValueError('demo agents only')
 return p.state['panes'][alias]

if operation=='info':
 product=p.run(['git','diff','--quiet',PRODUCT,'HEAD','--',*PRODUCT_FILES],cwd=p.state['plugin_root'],check=False)
 addresses=json.loads(p.namedrop('address','list','--json').stdout)['data']
 print(json.dumps({'workspace':p.state['workspace'],'panes':p.state['panes'],'profileHome':p.env['HOME'],'source':p.state['source'],
                   'version':p.namedrop_version(),'productRevision':PRODUCT,'productMatches':product.returncode==0,'addresses':addresses}))
elif operation=='open':
 result=p.namedrop('--herdr-pane',p.state['panes']['shell'],'channels','--no-open','--json')
 if result.returncode:raise RuntimeError(result.stdout)
 print(result.stdout)
elif operation=='status':
 print(p.herdr('pane','get',agent_pane(extra[0]))['pane'].get('agent_status'))
elif operation=='screen':
 print(p.run(['herdr','pane','read',agent_pane(extra[0]),'--source','visible']).stdout,end='')
elif operation=='ansi':
 print(p.run(['herdr','pane','read',agent_pane(extra[0]),'--source','visible','--ansi']).stdout,end='')
elif operation=='cancel':
 pane=agent_pane(extra[0])
 if p.herdr('pane','get',pane)['pane'].get('agent_status')!='blocked':
  raise ValueError('cancel requires a real blocked agent')
 print(p.run(['herdr','pane','send-keys',pane,'esc']).stdout,end='')
else:raise ValueError('unknown capture operation')
