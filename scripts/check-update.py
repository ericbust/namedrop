"""Detached, read-only GitHub version probe. Its only output is the local cache."""

import fcntl
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request

DAY_MS = 86_400_000
LIMIT = 65_536


def version(value):
    return (isinstance(value, str) and len(value) <= 32
            and re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value)
            and all(int(part) <= 2**64 - 1 for part in value.split('.')))


def read_json(path):
    with path.open('rb') as stream:
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError('JSON exceeds size limit')
    return json.loads(data)


def enabled(settings):
    try:
        value = read_json(settings)
        return (isinstance(value, dict) and set(value) <= {'check_for_updates'}
                and isinstance(value.get('check_for_updates', True), bool)
                and value.get('check_for_updates', True))
    except FileNotFoundError:
        return True
    except (OSError, ValueError):
        return False


def source_from_registration(value):
    plugins = value['result']['plugins']
    if value['result']['type'] != 'plugin_list' or len(plugins) != 1:
        raise ValueError('Invalid Namedrop registration')
    plugin = plugins[0]
    source = plugin['source']
    if plugin['plugin_id'] != 'namedrop' or source['kind'] != 'github':
        return None
    for key in ('owner', 'repo'):
        item = source[key]
        if (not isinstance(item, str) or len(item) > 100
                or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]*', item)
                or item in ('.', '..')):
            raise ValueError('Invalid GitHub source')
    ref = source['requested_ref']
    if (not isinstance(ref, str) or len(ref) > 255
            or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_./-]*', ref)
            or '..' in ref or '//' in ref or ref.endswith(('/', '.', '.lock'))):
        raise ValueError('Invalid installed ref')
    if re.fullmatch(r'[0-9a-fA-F]{40}|[0-9a-fA-F]{64}', ref):
        return None
    revision = source['resolved_commit']
    if not isinstance(revision, str) or not re.fullmatch(r'[0-9a-fA-F]{40}|[0-9a-fA-F]{64}', revision):
        raise ValueError('Invalid installed revision')
    root = Path(plugin['plugin_root'])
    managed = Path(source['managed_path'])
    if not root.is_absolute() or not managed.is_absolute() or not root.is_dir() or not managed.is_dir():
        raise ValueError('Invalid managed checkout')
    subdir = root.resolve().relative_to(managed.resolve()).as_posix()
    subdir = '' if subdir == '.' else subdir
    if (not re.fullmatch(r'[A-Za-z0-9_./-]*', subdir)
            or source.get('subdir') not in (None, subdir)):
        raise ValueError('Inconsistent installed subdirectory')
    return dict(owner=source['owner'], repo=source['repo'], requested_ref=ref,
                resolved_commit=revision.lower(), subdir=subdir)


def registration():
    output = run(
        [os.environ.get('HERDR_BIN_PATH', 'herdr'), 'plugin', 'list', '--plugin', 'namedrop', '--json'],
        timeout=5)
    return json.loads(output.stdout)


def remote_commit(source):
    ref = source['requested_ref']
    refs = [ref] if ref.startswith('refs/') else [f'refs/heads/{ref}', f'refs/tags/{ref}']
    if ref == 'HEAD':
        refs.insert(0, 'HEAD')
    patterns = [pattern for ref in refs for pattern in (ref, ref + '^{}')]
    environment = dict(os.environ, GIT_TERMINAL_PROMPT='0')
    output = run(
        ['git', '-c', 'credential.helper=', '-c', 'core.hooksPath=/dev/null', 'ls-remote', '--exit-code',
         f'https://github.com/{source["owner"]}/{source["repo"]}.git', *patterns],
        text=True, timeout=8, env=environment)
    found = dict()
    for line in output.stdout.splitlines():
        commit, name = line.split('\t')
        if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit):
            raise ValueError('Invalid remote commit')
        found[name] = commit
    for ref in refs:
        if ref in found:
            return found.get(ref + '^{}', found[ref])
    raise ValueError('Installed ref was not found')


def run(command, timeout, **options):
    with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, start_new_session=True, **options) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command)
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def remote_version(source, commit):
    prefix = source['subdir'] + '/' if source['subdir'] else ''
    url = f'https://raw.githubusercontent.com/{source["owner"]}/{source["repo"]}/{commit}/{prefix}herdr-plugin.toml'
    def deadline(signum, frame):
        raise TimeoutError('Remote manifest timed out')
    previous_handler = signal.signal(signal.SIGALRM, deadline)
    signal.alarm(10)
    try:
        with urllib.request.urlopen(url, timeout=8) as response:
            data = response.read(LIMIT + 1)
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, previous_handler)
    if len(data) > LIMIT:
        raise ValueError('Remote manifest exceeds size limit')
    # The manifest starts with scalar plugin metadata before its TOML tables.
    # Read only a literal numeric version; never evaluate downloaded content.
    header = re.split(r'(?m)^\s*\[', data.decode('utf-8'), maxsplit=1)[0]
    matches = re.findall(r'(?m)^version\s*=\s*"([^"\r\n]*)"\s*(?:#.*)?$', header)
    if len(matches) != 1 or not version(matches[0]):
        raise ValueError('Remote manifest has no numeric MAJOR.MINOR.PATCH version')
    return matches[0]


def atomic_write(path, value):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix='.update-check-', delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(value, stream)
            stream.write('\n')
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def check(state, settings, now=None, lookup_registration=registration,
          lookup_commit=remote_commit, lookup_version=remote_version):
    if not enabled(settings):
        return
    now = int(time.time() * 1000) if now is None else now
    state.mkdir(parents=True, exist_ok=True)
    with (state / 'update-check.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        path = state / 'update-check.json'
        try:
            previous = read_json(path)
            checked = previous['checked_at']
            if (previous.get('schema_version') == 1 and type(checked) is int
                    and 0 <= checked <= now and now - checked < DAY_MS):
                return
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            previous = {}
        cache = dict(schema_version=1, checked_at=now, source=None,
                     remote_commit=None, remote_version=None, last_success_at=None, last_error=None)
        try:
            source = source_from_registration(lookup_registration())
            cache['source'] = source
            if source is not None:
                if (previous.get('source') == source and version(previous.get('remote_version'))
                        and isinstance(previous.get('remote_commit'), str)
                        and re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', previous['remote_commit'])
                        and type(previous.get('last_success_at')) is int):
                    for key in ('remote_commit', 'remote_version', 'last_success_at'):
                        cache[key] = previous.get(key)
                commit = lookup_commit(source)
                available = cache['remote_version']
                if commit != cache['remote_commit'] or not version(available):
                    available = lookup_version(source, commit)
                if not version(available):
                    raise ValueError('Invalid remote version')
                cache.update(remote_commit=commit, remote_version=available, last_success_at=now)
        except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
            # Keep diagnostics bounded and printable; failures never reach a hook.
            cache['last_error'] = ''.join(c for c in str(error) if c.isprintable())[:512] or type(error).__name__
        atomic_write(path, cache)


if __name__ == '__main__':
    try:
        check(Path(sys.argv[1]), Path(sys.argv[2]))
    except (OSError, ValueError):
        pass
