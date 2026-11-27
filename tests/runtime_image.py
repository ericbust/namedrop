"""Move runtime/current while a real CLI waits for the command gate."""
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


def running_identity(pid, library):
    try:
        path = runtime.darwin_path(library, pid) if library else f'/proc/{pid}/exe'
        if path:
            info = os.stat(path)
            return info.st_dev, info.st_ino
    except FileNotFoundError:
        pass
    return None


def check(binary, root, keeps_old_image=False, unlink_old=False):
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
    library = runtime.darwin_library() if sys.platform == 'darwin' else None
    info = old.stat()
    identity = info.st_dev, info.st_ino

    with (rt / 'commands.lock').open('w') as gate:
        fcntl.flock(gate, fcntl.LOCK_EX)
        command = [launcher, '--plugin-state', state, '--json', 'describe']
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True) as child:
            try:
                deadline = time.monotonic() + 5
                while running_identity(child.pid, library) != identity:
                    assert child.poll() is None and time.monotonic() < deadline, 'old image did not start'
                    time.sleep(.01)
                staged = rt / 'next'
                staged.symlink_to(new.parent)
                os.replace(staged, current)
                if unlink_old:
                    old.unlink()
                    if sys.platform == 'linux':
                        assert running_identity(child.pid, library) == identity
                fcntl.flock(gate, fcntl.LOCK_UN)
                stdout, stderr = child.communicate(timeout=5)
                assert stdout.splitlines() == ['selected-image'], (child.returncode, stdout[:300], stderr[:300])
                if keeps_old_image:
                    assert child.returncode != 0 and 'still running' in stderr, (stdout, stderr)
                else:
                    assert child.returncode == 0, (stdout, stderr)
            finally:
                if child.poll() is None:
                    child.kill()


def check_firmlink(binary, root):
    state = root / 'state'
    rt = state / 'runtime'
    image = rt / 'releases' / ('a' * 64) / 'namedrop'
    image.parent.mkdir(parents=True)
    shutil.copy2(binary, image)
    launcher = rt / 'namedrop'
    launcher.write_text(runtime.LAUNCHER)
    launcher.chmod(0o700)
    (rt / 'current').symlink_to(image.parent.relative_to(rt))
    alias = Path('/System/Volumes/Data') / state.relative_to('/')
    assert os.path.samefile(state, alias)
    result = subprocess.run(
        [launcher, '--plugin-state', alias, '--json', 'describe'],
        capture_output=True, text=True, timeout=5,
    )
    assert result.returncode == 0, (result.returncode, result.stdout[:300], result.stderr)


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='namedrop-cli-image-', dir=Path(sys.argv[1]).resolve().parent) as scratch:
        root = Path(scratch).resolve()
        binary = Path(sys.argv[1])
        if sys.platform == 'darwin':
            check_firmlink(binary, root / 'firmlink')
        check(binary, root / 'switch')
        check(binary, root / 'wrong-image', keeps_old_image=True)
        if sys.platform == 'linux':
            check(binary, root / 'unlinked', unlink_old=True)
