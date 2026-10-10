"""Socket contract double forwarding validated actions to the existing fake host.

Copied as socket.py only into disposable setup/updater fixtures. UI focus is
deliberately irrelevant: the action must explicitly carry the original pane.
"""
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sysconfig

# pathlib.as_uri imports urllib and therefore the rest of socket on Python 3.14.
# Preserve that stdlib surface; replace only the Unix action connection below.
_native = runpy.run_path(str(Path(sysconfig.get_path('stdlib')) / 'socket.py'))
globals().update({key: value for key, value in _native.items() if not key.startswith('__')})


class socket:
    def __init__(self, family, kind):
        assert (family, kind) == (AF_UNIX, SOCK_STREAM)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def settimeout(self, timeout):
        assert 0 < timeout <= 10

    def connect(self, path):
        assert path == os.environ['HERDR_SOCKET_PATH']

    def sendall(self, data):
        request = json.loads(data)
        assert request['method'] == 'plugin.action.invoke'
        params = request['params']
        assert params['plugin_id'] == 'namedrop'
        assert params['context']['workspace_id'] == os.environ['HERDR_WORKSPACE_ID']
        assert params['context']['focused_pane_id'] == os.environ['HERDR_PANE_ID']
        if os.environ.get('NAMEDROP_TEST_SOCKET_REQUEST'):
            Path(os.environ['NAMEDROP_TEST_SOCKET_REQUEST']).write_bytes(data)
        result = subprocess.run(['herdr', 'plugin', 'action', 'invoke', params['action_id'],
                                 '--plugin', 'namedrop'], capture_output=True, check=True)
        self.response = dict(json.loads(result.stdout), id=request['id'])

    def makefile(self, mode):
        assert mode == 'rb'
        return io.BytesIO((json.dumps(self.response) + '\n').encode())
