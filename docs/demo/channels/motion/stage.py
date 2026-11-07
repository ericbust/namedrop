"""Stage a clean take in an owned test- profile: a new empty workspace named Release
planning, both agents started in ~/release-planning, warmed up and idle, Claude in manual mode."""
import json
import os
from pathlib import Path
import sys
import time

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(root / 'testbed/lib'))
import testbed
import livetest

WORKSPACE = 'Release planning'
NOTE = "When another agent answers your question, don't send it a thank-you."
name = sys.argv[1]
if not name.startswith('test-'):
    raise ValueError('staging requires a test- profile')
p = testbed.Profile(name)
p.owned()
assert p.socket.name.startswith('nd-tb-')
assert p.env['HOME'] != str(Path.home()) and p.env['XDG_CONFIG_HOME'] != str(Path.home() / '.config')

with p.locked():
    p.reset_repo()
    workspace = p.state.get('workspace')
    if workspace and p.herdr('workspace', 'get', workspace, check=False) is not None:
        p.herdr('workspace', 'close', workspace)
    p.clear_panes()
    p.workspace()
    p.panes(('claude', 'codex'), launch=False)
    p.herdr('workspace', 'rename', p.state['workspace'], WORKSPACE)

    home = Path(p.env['HOME'])
    demo = home / 'release-planning'
    if not demo.exists():
        p.run(['git', 'clone', '--quiet', str(p.repo), str(demo)])
    # A thank-you after an agent answers adds a message that names the person by their
    # Namedrop alias. The demo project asks for none, as any project may.
    for name in ('CLAUDE.md', 'AGENTS.md'):
        notes = demo / name
        lines = [line for line in (notes.read_text() if notes.exists() else '').splitlines() if not line.startswith('Call the person')]
        if NOTE not in lines:
            lines.append(NOTE)
        testbed.write_if_changed(notes, '\n'.join(lines) + '\n')
    claude_json = home / '.claude.json'
    version = p.tool_version('claude')
    state = json.loads(claude_json.read_text()) if claude_json.exists() else {}
    testbed.write_if_changed(claude_json, json.dumps(livetest.claude_state_seed(state, str(demo), version), indent=2) + '\n')
    config = p.dir / 'codex/config.toml'
    testbed.write_if_changed(config, livetest.codex_trust_seed(config.read_text() if config.exists() else '', str(demo)))

    r = livetest.Run(p, livetest.MODES['no-daemon'], p.dir / 'logs')
    for alias in ('claude', 'codex'):
        command = 'cd ~/release-planning && ' + livetest.launch_command(alias, r.mode, os.environ)
        p.launch(alias, command)
        first = r.settle_screens(alias)
        if first.restart:
            for _ in range(2):
                if r.panes()[alias].agent:
                    r.keys(alias, ('c-c',))
                if r.settle(lambda: r.panes()[alias].agent, lambda agent: agent is None, 10, watch=()) is None:
                    break
            p.state.pop('launched_' + alias, None)
            p.launch(alias, command)
            first = r.settle_screens(alias)
        print(alias, 'ready after', ', '.join(first.answered) or 'no screens', flush=True)
        print(alias, 'session', r.warm_up(alias), flush=True)
    # Claude starts in auto mode, which runs the Needs-you command without asking.
    # Shift-Tab cycles auto, manual, accept edits and plan; stop on manual.
    for _ in range(5):
        if 'manual mode on' in r.text('claude').rstrip().splitlines()[-1]:
            break
        p.run(['herdr', 'pane', 'send-keys', p.state['panes']['claude'], 'shift+tab'])
        time.sleep(1.5)
    else:
        raise RuntimeError('Claude did not reach manual permission mode')
    print('claude', r.text('claude').rstrip().splitlines()[-1].strip(), flush=True)
    addresses = json.loads(p.namedrop('address', 'list', '--json').stdout)['data']
    print(json.dumps([{key: row.get(key) for key in ('alias', 'registered', 'agent_status')} for row in addresses]))
