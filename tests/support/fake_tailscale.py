#!/usr/bin/python3
import json, pathlib, sys
root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if args == ['--probe']:
    sys.exit(0)
def read(name):
    return json.loads((root / name).read_text())
def write(name, value):
    (root / name).write_text(json.dumps(value))
if args == ['status', '--json']:
    print(json.dumps(read('identity.json')))
elif args == ['serve', 'status', '--json']:
    print(json.dumps(read('serve.json')))
else:
    state = read('state/dashboard/tailscale.json')
    with (root / 'changes.jsonl').open('a') as file:
        file.write(json.dumps({'args':args, 'state':state['state']}) + '\n')
    assert args[:2] == ['serve', '--bg']
    assert '--set-path=/' in args
    port = next(arg.split('=',1)[1] for arg in args if arg.startswith('--https='))
    target = next(arg for arg in args if arg.startswith('http://'))
    off = args[-1] == 'off'
    behavior = read('behavior.json')
    if behavior.get('fail_off' if off else 'fail_enable'):
        print('fixture command failed', file=sys.stderr)
        sys.exit(1)
    config = read('serve.json') or {}
    key = state['handler']['dns_name'] + ':' + port
    if off:
        config.get('TCP', {}).pop(port, None)
        config.get('Web', {}).pop(key, None)
        config.get('AllowFunnel', {}).pop(key, None)
    else:
        config.setdefault('TCP', {})[port] = {'HTTPS': True}
        config.setdefault('Web', {})[key] = {'Handlers': {'/': {'Proxy':target}}}
    if behavior.get('change_foreign'):
        config['unexpected'] = True
    write('serve.json', config)
