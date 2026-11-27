"""Move runtime/current while a real CLI waits for the command gate on macOS."""
import fcntl
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'scripts'))
import runtime


def check(binary, root, keeps_old_image):
    state = root / 'state'
    rt = state / 'runtime'
    old = rt / 'releases' / ('a' * 64) / 'namedrop'
    new = rt / 'releases' / ('b' * 64) / 'namedrop'
    old.parent.mkdir(parents=True)
    new.parent.mkdir(parents=True)
    shutil.copy2(binary, old)
    launcher = rt / 'namedrop'
    launcher.write_text(runtime.LAUNCHER)
    launcher.chmod(0o700)
    current = rt / 'current'
    current.symlink_to(old.parent)
    ends = f'exec {shlex.quote(str(old))} "$@"' if keeps_old_image else 'exit 0'
    new.write_text(f'#!/bin/sh\nprintf "%s\\n" selected-image\n{ends}\n')
    new.chmod(0o700)
    library = runtime.darwin_library()

    with (rt / 'commands.lock').open('w') as gate:
        fcntl.flock(gate, fcntl.LOCK_EX)
        command = [launcher, '--plugin-state', state, '--json', 'describe']
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True) as child:
            try:
                deadline = time.monotonic() + 5
                while runtime.darwin_path(library, child.pid) != str(old):
                    assert child.poll() is None and time.monotonic() < deadline, 'old image did not start'
                    time.sleep(.01)
                staged = rt / 'next'
                staged.symlink_to(new.parent)
                os.replace(staged, current)
                fcntl.flock(gate, fcntl.LOCK_UN)
                stdout, stderr = child.communicate(timeout=5)
                assert stdout.splitlines() == ['selected-image'], (stdout[:300], stderr[:300])
                if keeps_old_image:
                    assert child.returncode != 0 and 'still running' in stderr, (stdout, stderr)
                else:
                    assert child.returncode == 0, (stdout, stderr)
            finally:
                if child.poll() is None:
                    child.kill()


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='namedrop-cli-image-') as scratch:
        root = Path(scratch).resolve()
        for keeps_old_image in (False, True):
            check(Path(sys.argv[1]), root / str(keeps_old_image), keeps_old_image)
