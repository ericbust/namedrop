"""Standalone policy for the shared runtime publisher."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

from update import require, rex_session_panes, rex_server_check, preflight_panes, safe


class Install:
    def __init__(self, root, state, executable):
        self.root, self.state, self.executable = root, state, executable

    def check_owner(self):
        runtime = self.state / 'runtime'
        owner = runtime / 'owner'
        require(not owner.is_symlink(), f'Runtime file has an unexpected type: {owner}')
        value = owner.read_text().strip() if owner.exists() else 'herdr' if (runtime / 'current').exists() else None
        require(value in (None, 'herdr', 'standalone'), f'Invalid Namedrop runtime owner in {self.state}')
        require(value != 'herdr', "ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in "
                f'{self.state}. Run Activate Namedrop in Herdr to update them.')

    def survey(self):
        panes, stopped, unchecked = rex_session_panes(all_endpoints=True)
        endpoints = {p['_socket'] for p in panes if p['_socket'].startswith('rex:')}
        record = Path(os.environ['HOME']) / '.config/namedrop/rex.json'
        if record.exists():
            server = json.loads(record.read_text()).get('server')
            if server:
                require(isinstance(server, str) and server.startswith('rex:/'), 'Invalid Rex install server.')
                endpoints.add(server)
        server = os.environ.get('REX_SERVER', '')
        if server.startswith('unix://'):
            from urllib.parse import unquote
            target = unquote(server.removeprefix('unix://'))
        elif server:
            target = server
        elif sys.platform == 'darwin':
            target = str(Path(os.environ['HOME']) / 'Library/Application Support/rex/server.sock')
        else:
            directory = os.environ.get('XDG_RUNTIME_DIR')
            target = str(Path(directory) / 'rex/server.sock') if directory else ''
        require(os.path.isabs(target), 'Start Rex, or set REX_SERVER to its Unix socket.')
        endpoints.add('rex:' + target)
        checked = {endpoint: 'stopped' for endpoint in stopped}
        checked.update(unchecked)
        for endpoint in sorted(endpoints - checked.keys()):
            checked[endpoint] = rex_server_check(endpoint)
        stopped = sorted(endpoint for endpoint, result in checked.items() if result == 'stopped')
        unchecked = sorted((endpoint, result) for endpoint, result in checked.items() if result not in (None, 'stopped'))
        known = {pane['_socket'] for pane in panes}
        self.unknown_servers = [endpoint for endpoint, _ in unchecked if endpoint not in known]
        def key(pane):
            return (pane['_socket'], pane['workspace_id'], pane['pane_id'],
                    (pane.get('agent_session') or {}).get('value'))
        observations = {key(p): p for p in panes}
        for endpoint in sorted(endpoints):
            if endpoint in stopped or endpoint in dict(unchecked):
                continue
            args = [str(self.executable), '--plugin-state', str(self.state)]
            args += ['--endpoint', endpoint]
            result = subprocess.run([*args, 'rex', 'sessions', '--json'],
                                    capture_output=True, text=True, timeout=30)
            require(result.returncode == 0,
                    'Could not survey Rex agents before installation: ' + result.stderr.strip())
            value = json.loads(result.stdout)
            data = value.get('data', {})
            require(value.get('status') == 'ok' and value.get('operation') == 'rex.sessions'
                    and isinstance(data.get('panes'), list) and isinstance(data.get('endpoint'), str)
                    and data['endpoint'].startswith('rex:/')
                    and endpoint == data['endpoint'], 'Invalid Rex agent survey.')
            for pane in data['panes']:
                require(isinstance(pane, dict) and all(isinstance(pane.get(k), str) and pane[k]
                        for k in ('endpoint', 'space', 'block')) and pane['endpoint'] == data['endpoint']
                        and all(pane.get(k) is None or isinstance(pane[k], str)
                                for k in ('agent', 'session', 'status')), 'Invalid Rex block survey.')
                observed = dict(_socket=pane['endpoint'], workspace_id=pane['space'], pane_id=pane['block'],
                                agent=pane.get('agent'), agent_session={'value': pane['session']}
                                if pane.get('session') else None, agent_status=pane.get('status'))
                if observed['agent'] or observed['agent_session']:
                    observations[key(observed)] = observed
        return list(observations.values()), stopped, unchecked

    def session_panes(self):
        """The sessions of the one-time runtime migration. Only Namedrop's
        SessionStart hook binds a Rex agent's session, so a surveyed agent with
        none never loaded a Namedrop hook and has nothing to migrate. A later
        live update still waits for it, because nothing shows when it started."""
        panes, stopped, unchecked = self.survey()
        self.notices(stopped, unchecked)
        require(not self.unknown_servers,
                'Could not check the Rex server at ' + ', '.join(map(safe, self.unknown_servers))
                + ' for sessions to record in the one-time runtime migration. Install again when it answers.')
        return [pane for pane in panes if pane.get('agent_session')], []

    @staticmethod
    def notices(stopped, unchecked):
        from update import rex_notices
        for notice in rex_notices(dict(stopped_rex_servers=stopped, unchecked_rex_servers=unchecked)):
            print(notice, file=sys.stderr)

    def preflight(self, live=False):
        panes, stopped, unchecked = self.survey()
        return preflight_panes(panes, self.unknown_servers, stopped, unchecked, live)

    @staticmethod
    def refusal(state, agents):
        """Why a reinstall waits, and each session and server it waits for."""
        if state['live_update']:
            lines = ['Some Rex sessions still use pre-launcher tools or incomplete setup. Finish their work and '
                     'restart the agents before installing again; nothing was stopped.']
        else:
            lines = ['This update needs a session refresh. Finish the work in each Rex session below and exit its '
                     'agent before installing again; nothing was stopped.']
        for pane in agents:
            name = '@' + pane['label'] if pane.get('label') else 'An agent'
            lines.append(f"- {name} ({pane.get('agent') or 'unknown kind'}, {pane.get('agent_status') or 'unknown'})"
                         f" in block {pane['pane_id']} on {pane['_socket']}")
        lines += [f'- The Rex server at {server}, which could not be checked' for server in state['other_servers']]
        if not state['live_update'] and (state['unfinished'] or state['joins']):
            lines.append(f"- {state['unfinished']} unfinished handoffs and {state['joins']} combined results pending")
        return '\n  '.join(safe(line) for line in lines)

    def verify(self, payload):
        data = payload.get('data', {})
        packages = data.get('integration_packages', {})
        require(payload.get('status') == 'ok' and payload.get('operation') == 'install'
                and data.get('state_directory') == str(self.state)
                and isinstance(packages, dict) and packages
                and all(package.get('installed') is True for package in packages.values()),
                'Installation did not verify this state and integration packages.')
        record = json.loads((Path(os.environ['HOME']) / '.config/namedrop/rex.json').read_text())
        require(record.get('state') == str(self.state)
                and isinstance(record.get('launcher'), str) and Path(record['launcher']).is_absolute()
                and Path(record['launcher']).resolve() == (self.state / 'runtime/namedrop').resolve()
                and record.get('module') == data.get('module')
                and record.get('config') == data.get('config')
                and record.get('line') == data.get('line')
                and isinstance(record.get('server'), str) and record['server'].startswith('rex:/')
                and isinstance(record.get('module'), str) and Path(record['module']).is_absolute()
                and Path(record['module']).is_file()
                and (data.get('confirmed') is True or data.get('manual_config') is True),
                'Installation did not verify the Rex registration and module.')

    def source(self):
        manifest = (self.root / 'herdr-plugin.toml').read_text()
        version = re.search(r'^version\s*=\s*"([^"]+)"', manifest, re.M)
        require(version is not None, 'Install source has no release version.')
        kind, origin = 'archive', str(self.root)
        if (self.root / '.git').exists():
            kind = 'checkout'
            result = subprocess.run(['git', '-C', str(self.root), 'remote', 'get-url', 'origin'],
                                    capture_output=True, text=True, timeout=10)
            if result.returncode == 0:
                origin = public(result.stdout.strip())
        return dict(version=version[1], kind=kind, origin=origin)


def public(origin):
    """`origin` without credentials: no password, and no user name over HTTP,
    where a token can stand in for one."""
    from urllib.parse import urlsplit, urlunsplit
    parts = urlsplit(origin)
    if '@' not in parts.netloc:
        return origin
    user, _, host = parts.netloc.rpartition('@')
    user = '' if parts.scheme in ('http', 'https') else user.partition(':')[0]
    return urlunsplit(parts._replace(netloc=user + '@' + host if user else host))
