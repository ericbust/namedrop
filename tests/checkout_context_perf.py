"""Checkout-context performance probe on a real Herdr server and real Namedrop CLI.

Times the default review handoff (send with pinned Git context, the reviewer's
bound context reads and reply, then handoff results by message_id) against a
frozen fixture: repository A with 100 tracked files, 10 dirty tracked files and
10 untracked files, plus a registered linked worktree B. Agents are simulated
as in coordination_native.py; no model or credentials are used.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

from coordination_native import RENDERER, cleanup_status, isolated_env, read_envelopes
from seamless_native import FIXTURE_ROOT, compile_agent

REPO = Path(__file__).resolve().parent.parent
TRACKED, DIRTY, UNTRACKED = 100, 10, 10
REVIEWED_PATHS = [f'src/file-{i:03d}.txt' for i in range(3)]
METRIC = ('Wall time from immediately before the worker send CLI invocation until the worker handoff.results '
          'response is parsed: send (context, capture_git_state true, diff evidence for the reviewed paths), '
          'wait for the reviewer delivery envelope, every pre-bound context.read in it, bound reply, '
          'handoff.results by message_id. Excludes compilation, server startup, fixture reset and think time.')


def wait(predicate, description, timeout=35, interval=.15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    raise AssertionError('Timed out: ' + description)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def nearest_rank_percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


def fixture_processes(base):
    found = {}
    marker = str(base).encode()
    for entry in Path('/proc').iterdir() if Path('/proc').is_dir() else ():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            cmdline = (entry/'cmdline').read_bytes()
            hit = marker in cmdline or marker in os.readlink(entry/'cwd').encode() or marker in (entry/'environ').read_bytes()
        except OSError:
            continue
        if hit:
            found[entry.name] = cmdline.replace(b'\0', b' ').decode(errors='replace').strip()
    return found


class Fixture:
    PREFIX = 'namedrop-perf'
    AGENTS = ('worker', 'reviewer')

    def __init__(self, side, binary, herdr, log):
        self.side, self.binary, self.herdr, self.log = side, binary, herdr, log
        self.base = Path(tempfile.mkdtemp(prefix=f'{self.PREFIX}-{side}-', dir=FIXTURE_ROOT))
        for name in ('home', 'config', 'state', 'data', 'run', 'plugin'):
            (self.base/name).mkdir()
        self.env = isolated_env(self.base)
        self.repo_a, self.repo_b = self.base/'repo-a', self.base/'repo-b'
        self.state = self.base/'state/herdr/plugins/namedrop'
        self.launcher = self.state/'runtime/namedrop'
        self.phase, self.pending = 'setup', []
        self.pid_files, self.server = {}, None

    def command(self, *argv, cwd=None, context=None, timeout=60, check=True):
        started = time.monotonic_ns()
        result = subprocess.run([str(a) for a in argv], env=dict(self.env, **(context or {})),
                                cwd=cwd or self.repo_a, capture_output=True, text=True, timeout=timeout)
        self.pending.append({'side': self.side, 'phase': self.phase, 'elapsed_ns': time.monotonic_ns() - started,
                             'args': [str(a) for a in argv], 'cwd': str(cwd or self.repo_a), 'exit': result.returncode,
                             'stdout': result.stdout, 'stderr': result.stderr})
        assert not check or result.returncode == 0, self.pending[-1]
        return result.stdout

    def flush(self):
        with self.log.open('a') as log:
            for row in self.pending:
                log.write(json.dumps(row) + '\n')
        self.pending = []

    def host(self, *argv, context=None):
        output = self.command(self.herdr, *argv, context=context)
        return json.loads(output) if output.strip() else None

    def git(self, *argv, repo=None):
        return self.command('git', '-c', 'user.name=Namedrop perf', '-c', 'user.email=perf@example.invalid',
                            '-C', repo or self.repo_a, *argv).strip()

    def build_repositories(self):
        self.repo_a.mkdir()
        self.git('init', '--quiet', '--initial-branch=main')
        (self.repo_a/'src').mkdir()
        for i in range(TRACKED):
            (self.repo_a/f'src/file-{i:03d}.txt').write_text(f'tracked file {i}\n' + 'line\n' * 8)
        self.git('add', '.')
        self.git('commit', '--quiet', '-m', 'Perf fixture')
        for i in range(DIRTY):
            with (self.repo_a/f'src/file-{i:03d}.txt').open('a') as file:
                file.write(f'dirty-A {i}\n')
        (self.repo_a/'untracked').mkdir()
        for i in range(UNTRACKED):
            (self.repo_a/f'untracked/new-{i:02d}.txt').write_text(f'untracked-A {i}\n')
        self.git('worktree', 'add', '--quiet', '-b', 'perf-b', self.repo_b)
        porcelain = self.command('git', '-C', self.repo_a, 'status', '--porcelain')
        assert len([l for l in porcelain.splitlines() if l.startswith(' M')]) == DIRTY, porcelain
        assert len([l for l in porcelain.splitlines() if l.startswith('??')]) == 1, porcelain  # untracked/ dir
        assert len(self.git('worktree', 'list', '--porcelain').split('\n\n')) == 2

    def repository_digest(self):
        return {'head': self.git('rev-parse', 'HEAD'), 'status': self.git('status', '--porcelain=v2', '-uall'),
                'index_sha256': sha256(self.repo_a/'.git/index'),
                'worktrees': self.git('worktree', 'list', '--porcelain')}

    def start(self):
        self.build_repositories()
        self.repository_before = self.repository_digest()
        root = self.base/'plugin'
        (root/'scripts').mkdir()
        (root/'bin').mkdir()
        for name in ('setup.sh', 'run.sh', 'runtime.py', 'update.py'):
            shutil.copyfile(REPO/'scripts'/name, root/'scripts'/name)
        for name in ('herdr-plugin.toml', 'runtime.json'):
            shutil.copyfile(REPO/name, root/name)
        shutil.copyfile(self.binary, root/'bin/namedrop')
        (root/'bin/namedrop').chmod(0o700)
        self.server_log = (self.base/'server.log').open('w')
        self.server = subprocess.Popen([self.herdr, 'server'], env=self.env, cwd=self.repo_a, stdout=self.server_log,
                                       stderr=self.server_log, start_new_session=True)
        wait(lambda: (self.base/'run/herdr.sock').exists(), 'disposable server socket')
        created = self.host('workspace', 'create', '--cwd', self.repo_a, '--label', 'Checkout perf')['result']
        self.workspace = created['workspace']['workspace_id']
        # Only the focused root pane toggles status; an unfocused tab that stops
        # working is reported as done rather than idle.
        self.panes = {'worker': created['root_pane']['pane_id']}
        for name in self.AGENTS[1:]:
            self.panes[name] = self.host('tab', 'create', '--workspace', self.workspace, '--cwd', self.repo_a,
                                         '--label', f'Perf {name}')['result']['root_pane']['pane_id']
        self.cwd = {name: self.repo_b if name == 'worker' else self.repo_a for name in self.panes}
        self.context = dict(HERDR_ENV='1', HERDR_WORKSPACE_ID=self.workspace, HERDR_PANE_ID=self.panes['worker'],
                            HERDR_PLUGIN_STATE_DIR=str(self.state))
        self.host('plugin', 'link', root, context=self.context)
        self.command('sh', root/'scripts/setup.sh', 'activate', '--json', context=self.context)
        assert sha256(self.state/'runtime/current/namedrop') == sha256(self.binary)
        simulator = self.base/'codex'
        compile_agent(simulator)
        (self.base/'render.py').write_text(RENDERER)
        self.sessions = {name: f'{i:08d}-2222-4222-8222-{i:012d}' for i, name in enumerate(self.panes, 1)}
        self.inboxes = {name: self.base/f'{name}-envelopes.jsonl' for name in self.panes}
        self.surfaces = {name: self.base/f'{name}-surface' for name in self.panes}
        for name, pane in self.panes.items():
            self.surfaces[name].write_text('idle')
            wait(lambda: (self.base/'home'/f'shell-ready-{pane}').exists(), 'fixture shell prompt')
            script = self.base/f'start-{name}.sh'
            self.pid_files[name] = self.base/f'{name}-pids'
            script.write_text('#!/bin/sh\nstty -icanon -echo min 1 time 0\n'
                              + shlex.join([sys.executable, str(self.base/'render.py'), str(self.surfaces[name])]) + ' &\n'
                              + 'printf \'%s\\n\' "$$" "$!" > ' + shlex.quote(str(self.pid_files[name])) + '\nexec '
                              + shlex.quote(str(simulator)) + ' >> ' + shlex.quote(str(self.inboxes[name])) + '\n')
            self.host('pane', 'run', pane, shlex.join(['sh', str(script)]))
            wait(lambda: self.pid_files[name].exists() and len(self.pid_files[name].read_text().splitlines()) == 2,
                 'owned simulator and renderer PID handles')
            wait(lambda: any(p['pane_id'] == pane and p.get('agent') == 'codex' for p in self.live()), 'simulator detection')
            self.host('pane', 'report-agent-session', pane, '--source', 'herdr:codex', '--agent', 'codex',
                      '--agent-session-id', self.sessions[name])
            self.report(name, 'working' if name == 'worker' else 'idle')
            self.namedrop(name, 'name', name, '--type', 'agent')
        self.flush()

    def live(self):
        return self.host('pane', 'list')['result']['panes']

    def report(self, name, status):
        pane = self.panes[name]
        previous = next(p for p in self.live() if p['pane_id'] == pane)['agent_status']
        started = time.time_ns()
        staged = self.surfaces[name].with_suffix('.next')
        staged.write_text(status)
        staged.replace(self.surfaces[name])
        wait(lambda: any(p['pane_id'] == pane and p.get('agent_status') == status
                         and (p.get('agent_session') or {}).get('value') == self.sessions[name] for p in self.live()),
             f'{name} detected {status} with original session')
        if previous != status:
            def hook_finished():
                logs = self.host('plugin', 'log', 'list', '--plugin', 'namedrop', '--limit', '100')['result']['logs']
                return any(row.get('event') == 'pane.agent_status_changed' and row.get('exit_code') == 0
                           and row['started_unix_ms'] >= started // 1_000_000 for row in logs)
            wait(hook_finished, 'status-change hook completed')
            wait(lambda: not list((self.state/'agent-status-reconciliation').glob('*/pending-*')),
                 'status reconciliation processed')

    def namedrop(self, name, *argv, payload=None):
        if payload is not None:
            argv = (*argv, '--input-json', json.dumps(payload))
        response = json.loads(self.command(self.launcher, '--plugin-state', self.state, *argv, cwd=self.cwd[name],
                                           context=dict(self.context, HERDR_PANE_ID=self.panes[name])))
        assert response.get('status') == 'ok', response
        return response['data']

    def bound(self, name, envelope, operation, **authored):
        tool = envelope['control']['tool']
        bound_env = tool['environment']
        assert Path(tool['executable']) == self.launcher and bound_env['HERDR_PANE_ID'] == self.panes[name]
        argv = [tool['executable'], '--plugin-state', bound_env['HERDR_PLUGIN_STATE_DIR'], *operation['argv'],
                tool['input_flag'], json.dumps(dict(operation['input'], **authored)),
                '--herdr-socket', bound_env['HERDR_SOCKET_PATH'], '--herdr-workspace', bound_env['HERDR_WORKSPACE_ID'],
                '--herdr-pane', bound_env['HERDR_PANE_ID']]
        result = json.loads(self.command(*argv, cwd=self.cwd[name], context=bound_env))
        assert result.get('status') == 'ok', result
        return result['data']

    def envelope(self, name, message_id, interval=.15):
        return wait(lambda: next((e for e in read_envelopes(self.inboxes[name])
                                  if e['control']['message_id'] == message_id), None),
                    f'{name} envelope for {message_id}', interval=interval)

    def pin_refs(self):
        return self.git('for-each-ref', '--format=%(refname)', 'refs/herdr-context/').split()

    def reset(self, reply_message_id):
        self.report('worker', 'idle')
        receipt = self.envelope('worker', reply_message_id)
        self.report('worker', 'working')
        self.bound('worker', receipt, next(a for a in receipt['control']['tool']['operations']
                                           if a['token'] == 'complete.no-reply'))
        refs = self.pin_refs()
        for ref in refs:
            self.git('update-ref', '-d', ref)
        assert not self.pin_refs()
        for name, inbox in self.inboxes.items():
            with (self.log.parent/f'{self.side}-{name}-envelopes.jsonl').open('a') as kept:
                kept.write(inbox.read_text())
            inbox.write_text('')  # The simulator appends with O_APPEND, so truncation is safe.
        self.flush()

    def close(self):
        if self.server is not None:
            if self.server.poll() is None:
                self.server.terminate()
            try:
                self.server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait(timeout=5)
            self.server_log.close()
            shutil.copyfile(self.base/'server.log', self.log.parent/f'{self.side}-server.log')
        owned = {'server': self.server.pid} if self.server else {}
        missing = []
        for name, path in self.pid_files.items():
            try:
                cat, renderer = map(int, path.read_text().splitlines())
                owned.update({name+'-cat': cat, name+'-renderer': renderer})
            except (OSError, ValueError):
                missing.append(str(path))
        sockets = [self.base/'run/herdr.sock', self.base/'run/herdr-client.sock']

        def status():
            result = cleanup_status(owned, sockets, missing)
            stray = fixture_processes(self.base)
            return dict(result, verified=result['verified'] and not stray, fixture_processes=stray)
        deadline = time.monotonic() + 10
        cleanup = status()
        while not cleanup['verified'] and time.monotonic() < deadline:
            time.sleep(.15)
            cleanup = status()
        self.flush()
        return dict(cleanup, scope=str(self.base), owned_pids=owned,
                    server_exit=self.server.returncode if self.server else None,
                    proc_scan='linux /proc cmdline, cwd and environ' if Path('/proc').is_dir() else 'unavailable')


def review_context(checkout=None):
    context = {'capture_git_state': True, 'snapshot': {
        'objective': 'Review the perf fixture change', 'applicable_user_instructions': [],
        'decisions': [], 'completed_work': ['Edited fixture files'], 'current_work': ['Awaiting review'],
        'blockers': [], 'evidence': [{'kind': 'diff', 'locator': path} for path in REVIEWED_PATHS]}}
    if checkout is not None:
        context['checkout'] = {'path': str(checkout)}
    return context


def default_workflow(fx, tag, checkout=None, captured='dirty-A', excluded='dirty-B'):
    send_input = {'request_id': f'perf-{tag}-send', 'author_kind': 'agent_session',
                  'source': '@reviewer Review the perf fixture change.', 'context': review_context(checkout)}
    steps = []

    def step(name, run):
        started = time.monotonic_ns()
        value = run()
        steps.append({'step': name, 'ns': time.monotonic_ns() - started})
        return value
    started = time.monotonic_ns()
    sent = step('send', lambda: fx.namedrop('worker', 'send', payload=send_input))
    envelope = step('delivery_wait', lambda: fx.envelope('reviewer', sent['message_id'], interval=.002))
    operations = envelope['control']['tool']['operations']
    reads = [step('context.read', lambda op=op: fx.bound('reviewer', envelope, op))
             for op in operations if op['token'] == 'context.read']
    reply = step('reply', lambda: fx.bound('reviewer', envelope, next(op for op in operations if op['token'] == 'reply'),
                                           source='@worker Reviewed the pinned diff.'))
    results = step('handoff.results', lambda: fx.namedrop('worker', 'handoff', 'results',
                                                         payload={'message_id': sent['message_id']}))
    elapsed = time.monotonic_ns() - started
    pinned = [r for r, op in zip(reads, (op for op in operations if op['token'] == 'context.read'))
              if op['input']['selection']['kind'] == 'git_state']
    assert len(pinned) == len(REVIEWED_PATHS), [op['input'] for op in operations if op['token'] == 'context.read']
    assert all(captured in json.dumps(r) and excluded not in json.dumps(r) for r in pinned), \
        f'Pinned reads must show {captured} work and no {excluded} work'
    recipients = results['recipients']
    assert [r['result_status'] for r in recipients] == ['replied'], recipients
    assert recipients[0]['freshness']['status'] == 'fresh', recipients[0]['freshness']
    return {'elapsed_ns': elapsed, 'steps': steps, 'message_id': sent['message_id'],
            'reply_message_id': reply['message_id'], 'context_reads': len(reads)}


def explicit_preview_workflow(fx, tag):
    started = time.monotonic_ns()
    preview = fx.namedrop('worker', 'handoff', 'preview', payload={'source': '@reviewer Review the perf fixture change.',
                                                                   'context': review_context(fx.repo_b)})
    elapsed = time.monotonic_ns() - started
    assert preview['context']['snapshot']['checkout']['root'] == os.path.realpath(fx.repo_b), preview['context']
    return {'elapsed_ns': elapsed, 'steps': [{'step': 'handoff.preview', 'ns': elapsed}]}


def selected_b_workflow(fx, tag):
    return default_workflow(fx, tag, checkout=fx.repo_b, captured='dirty-B', excluded='dirty-A')


def dirty_checkout_b(fx):
    for i in range(DIRTY):
        with (fx.repo_b/f'src/file-{i:03d}.txt').open('a') as file:
            file.write(f'dirty-B {i}\n')
    (fx.repo_b/'untracked').mkdir()
    for i in range(UNTRACKED):
        (fx.repo_b/f'untracked/new-{i:02d}.txt').write_text(f'untracked-B {i}\n')


def head_only_workflows():
    return {'explicit_b_preview': explicit_preview_workflow, 'selected_b_review': selected_b_workflow}


HEAD_ONLY_BUDGET_MS = {'explicit_b_preview': 250, 'selected_b_review': 3000}


def summarize(samples):
    raw = [s['elapsed_ns'] for s in samples]
    return {'count': len(raw), 'raw_ns': raw, 'p50_ms': nearest_rank_percentile(raw, .5) / 1e6, 'p95_ms': nearest_rank_percentile(raw, .95) / 1e6,
            'min_ms': min(raw) / 1e6, 'max_ms': max(raw) / 1e6, 'mean_ms': sum(raw) / len(raw) / 1e6,
            'step_p50_ms': {name: nearest_rank_percentile([sum(x['ns'] for x in s['steps'] if x['step'] == name) for s in samples], .5) / 1e6
                            for name in dict.fromkeys(x['step'] for x in samples[0]['steps'])},
            'samples': samples}


def run_sample(fx, workflow, phase, index):
    fx.phase = f'{phase}-{index}'
    assert not fx.pin_refs()
    fx.flush()
    sample = workflow(fx, f'{fx.side}-{phase}-{index}')
    fx.phase = f'reset-{phase}-{index}'
    if 'reply_message_id' in sample:
        fx.reset(sample['reply_message_id'])
    else:
        assert not fx.pin_refs(), 'A preview must not pin'
        fx.flush()
    return dict(sample, side=fx.side, phase=phase, index=index)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline-binary', type=Path, required=True)
    parser.add_argument('--head-binary', type=Path)
    parser.add_argument('--baseline-only', action='store_true')
    parser.add_argument('--evidence-dir', type=Path)
    parser.add_argument('--samples', type=int, default=40)
    parser.add_argument('--warmup', type=int, default=3)
    args = parser.parse_args()
    assert args.baseline_only != bool(args.head_binary), 'Pass exactly one of --baseline-only and --head-binary'
    herdr = shutil.which('herdr')
    assert herdr, 'Native Herdr is required'
    binaries = {'baseline': args.baseline_binary.resolve()}
    if args.head_binary:
        binaries['head'] = args.head_binary.resolve()
    evidence = (args.evidence_dir or REPO/'.scratch/checkout-context-perf'/time.strftime('%Y%m%dT%H%M%S')).resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    log = evidence/'commands.jsonl'
    harness = Path(__file__).resolve()
    report = {
        'metric': METRIC, 'percentile_method': 'nearest-rank', 'invocation': [sys.executable, *sys.argv],
        'head_only_metrics': {
            'explicit_b_preview': 'One worker handoff.preview whose context selects checkout B, from before the CLI '
                                  'invocation until its response is parsed. Nothing is written.',
            'selected_b_review': 'The default workflow with context.checkout naming B, after B gets the same shape of '
                                 'dirty and untracked work as A.'},
        'harness': {'path': str(harness), 'sha256': sha256(harness),
                    'git_sha': subprocess.run(['git', '-C', REPO, 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip(),
                    'git_status': subprocess.run(['git', '-C', REPO, 'status', '--porcelain'], capture_output=True, text=True).stdout},
        'host': {'platform': platform.platform(), 'python': sys.version.split()[0],
                 'herdr': subprocess.run([herdr, '--version'], capture_output=True, text=True).stdout.strip(),
                 'shell': os.environ.get('NAMEDROP_TEST_SHELL')},
        'fixture': {'tracked_files': TRACKED, 'dirty_tracked_files': DIRTY, 'untracked_files': UNTRACKED,
                    'registered_worktrees': 2, 'reviewed_paths': REVIEWED_PATHS,
                    'reset': 'settle the returned reply, delete refs/herdr-context pins, truncate simulator inboxes'},
        'binaries': {side: {'path': str(path), 'sha256': sha256(path),
                            'version': subprocess.run([path, '--version'], capture_output=True, text=True).stdout.strip()}
                     for side, path in binaries.items()},
        'samples_requested': args.samples, 'warmup': args.warmup,
    }
    fixtures = {side: Fixture(side, path, herdr, log) for side, path in binaries.items()}
    results, cleanup, success = {'default': {side: [] for side in fixtures}}, {}, False
    try:
        for fx in fixtures.values():
            fx.start()
        report['fixture'].update({side: {'scope': str(fx.base), 'workspace': fx.workspace, 'panes': fx.panes,
                                         'sessions': fx.sessions, 'repository': fx.repository_before}
                                  for side, fx in fixtures.items()})
        for fx in fixtures.values():
            for i in range(args.warmup):
                run_sample(fx, default_workflow, 'warmup', i)
        for i in range(args.samples):
            for fx in (list(fixtures.values()) if i % 2 == 0 else list(fixtures.values())[::-1]):
                results['default'][fx.side].append(run_sample(fx, default_workflow, 'sample', i))
        if 'head' in fixtures:
            dirty_checkout_b(fixtures['head'])
            for name, workflow in head_only_workflows().items():
                for i in range(args.warmup):
                    run_sample(fixtures['head'], workflow, f'warmup-{name}', i)
                results[name] = {'head': [run_sample(fixtures['head'], workflow, name, i) for i in range(args.samples)]}
        for fx in fixtures.values():
            after = fx.repository_digest()
            assert after == fx.repository_before, 'Capture must not change the repository, index or worktrees'
        report['workflows'] = {name: {side: summarize(samples) for side, samples in sides.items()}
                               for name, sides in results.items()}
        if 'head' in fixtures:
            base, head = (report['workflows']['default'][s]['p95_ms'] for s in ('baseline', 'head'))
            limit = max(1.25 * base, base + 25)
            report['rule'] = {'default_p95_limit_ms': limit, 'head_p95_ms': head, 'pass': head <= limit}
            report['head_only_rules'] = {
                name: {'p95_ms': report['workflows'][name]['head']['p95_ms'], 'limit_ms': budget,
                       'pass': report['workflows'][name]['head']['p95_ms'] <= budget}
                for name, budget in HEAD_ONLY_BUDGET_MS.items()}
        success = True
    finally:
        for side, fx in fixtures.items():
            cleanup[side] = fx.close()
        report['cleanup'] = cleanup
        report['scenario_success'] = success
        name = 'perf-baseline.json' if args.baseline_only else 'perf-compare.json'
        (evidence/name).write_text(json.dumps(report, indent=2) + '\n')
        verified = all(c['verified'] for c in cleanup.values())
        if success and verified:
            for fx in fixtures.values():
                shutil.rmtree(fx.base)
        else:
            print('Fixture retained:', *(fx.base for fx in fixtures.values()), file=sys.stderr)
        print('Evidence:', evidence/name)
        for name, sides in (report.get('workflows') or {}).items():
            for side, summary in sides.items():
                print(f"{side}: {name} p50 {summary['p50_ms']:.1f} ms, p95 {summary['p95_ms']:.1f} ms, n={summary['count']}")
        if success and not verified:
            raise AssertionError(f'Perf run passed but cleanup is incomplete: {cleanup}')
    failed = [rule for rule in [report.get('rule'), *(report.get('head_only_rules') or {}).values()] if rule and not rule['pass']]
    if failed:
        sys.exit('p95 exceeds the budget: ' + json.dumps(failed))


if __name__ == '__main__':
    main()
