"""Checkout-context live lanes on a real Herdr server and the real Namedrop CLI.

Each scenario builds repository A, a registered linked worktree B, a Herdr space attached to A and
simulated agents (see coordination_native.py), then drives the real send, bound envelope actions,
context reads and handoff.results. No model, credentials or user profile is used, and nothing on the
decisive path writes the state database directly. Every fixture is disposable; a failed lane keeps it.

    NAMEDROP_TEST_SHELL=bash python3 tests/checkout_context_native.py --binary <namedrop> \\
        --scenario <name> --evidence-dir <dir> [--baseline-binary <trunk namedrop>]

--baseline-binary also runs the trunk parts of default-and-explicit, grant-session and protected-evidence.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys

from checkout_context_perf import Fixture, sha256, wait

A_SIDE, B_SIDE = 'A-SIDE edit', 'B-SIDE edit'
REVIEWED = 'src/shared.txt'
STATE_TABLES = ('working_context_snapshots', 'context_grants', 'evidence_index', 'collaboration_messages',
                'mailbox_deliveries', 'delivery_attempts', 'message_context_attachments', 'idempotency_records')
QUIET_GIT = ('--no-optional-locks', '-c', 'core.fsmonitor=false', '-c', 'filter.marker.clean=cat',
             '-c', 'filter.marker.smudge=cat')


class Failed(AssertionError):
    pass


class Lane(Fixture):
    PREFIX = 'namedrop-lane'

    def __init__(self, side, binary, herdr, evidence, agents):
        self.AGENTS = agents
        self.evidence = evidence
        evidence.mkdir(parents=True, exist_ok=True)
        super().__init__(side, binary, herdr, evidence.parent/'commands.jsonl')
        self.console_runs = 0

    def build_repositories(self):
        a = self.repo_a
        a.mkdir()
        self.git('init', '--quiet', '--initial-branch=main')
        write(a/'src/shared.txt', 'shared base\n')
        write(a/'src/other.txt', 'other base\n')
        write(a/'src/staged.txt', 'staged base\n')
        write(a/'README', 'checkout context lane fixture\n')
        self.git('add', '.')
        self.git('commit', '--quiet', '-m', 'Lane fixture')
        self.git('worktree', 'add', '--quiet', '-b', 'lane-b', self.repo_b)
        write(a/REVIEWED, A_SIDE + '\n')
        write(self.repo_b/REVIEWED, B_SIDE + '\n')

    def start(self):
        super().start()
        self.console_pane = self.host('tab', 'create', '--workspace', self.workspace, '--cwd', self.repo_a,
                                      '--label', 'Lane console', '--no-focus')['result']['root_pane']['pane_id']
        wait(lambda: (self.base/'home'/f'shell-ready-{self.console_pane}').exists(), 'console shell prompt')
        self.flush()

    def save(self, name, value):
        path = self.evidence/name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value if isinstance(value, str) else json.dumps(value, indent=2) + '\n')

    def quiet_git(self, repo, *argv, check=True):
        return self.command('git', *QUIET_GIT, '-C', repo, *argv, check=check).strip()

    def digest(self, repo):
        index = Path(self.quiet_git(repo, 'rev-parse', '--path-format=absolute', '--git-path', 'index'))
        return {'head': self.quiet_git(repo, 'rev-parse', 'HEAD'),
                'ref': self.quiet_git(repo, 'symbolic-ref', '-q', 'HEAD', check=False),
                'index_sha256': sha256(index) if index.exists() else None,
                'status': self.quiet_git(repo, 'status', '--porcelain=v2', '-uall', '--ignored')}

    def digests(self):
        return {'A': self.digest(self.repo_a), 'B': self.digest(self.repo_b) if self.repo_b.exists() else None,
                'pins': self.pin_refs()}

    def counts(self):
        with sqlite3.connect((self.state/'namedrop.sqlite3').as_uri() + '?mode=ro', uri=True) as db:
            rows = {t: db.execute(f'SELECT count(*) FROM {t}').fetchone()[0] for t in STATE_TABLES}
        objects = self.quiet_git(self.repo_a, 'count-objects', '-v')
        return dict(rows, pins=self.pin_refs(), objects=objects)

    def call(self, name, *argv, payload=None):
        if payload is not None:
            argv = (*argv, '--input-json', json.dumps(payload))
        return json.loads(self.command(self.launcher, '--plugin-state', self.state, *argv, cwd=self.cwd[name],
                                       context=dict(self.context, HERDR_PANE_ID=self.panes[name]), check=False))

    def run_bound(self, envelope, argv, body, as_name=None, check=True):
        tool = envelope['control']['tool']
        env = dict(tool['environment'])
        assert Path(tool['executable']) == self.launcher and env['HERDR_SOCKET_PATH'] == self.env['HERDR_SOCKET_PATH']
        owner = next(n for n, p in self.panes.items() if p == env['HERDR_PANE_ID'])
        if as_name:
            env['HERDR_PANE_ID'] = self.panes[as_name]
        command = [tool['executable'], '--plugin-state', env['HERDR_PLUGIN_STATE_DIR'], *argv, tool['input_flag'],
                   json.dumps(body), '--herdr-socket', env['HERDR_SOCKET_PATH'],
                   '--herdr-workspace', env['HERDR_WORKSPACE_ID'], '--herdr-pane', env['HERDR_PANE_ID']]
        response = json.loads(self.command(*command, cwd=self.cwd[as_name or owner], context=env, check=False))
        if check:
            assert response.get('status') == 'ok', response
        return response

    def operations(self, envelope, token):
        return [op for op in envelope['control']['tool']['operations'] if op['token'] == token]

    def action(self, envelope, token, check=True, **authored):
        op = self.operations(envelope, token)[0]
        return self.run_bound(envelope, op['argv'], dict(op['input'], **authored), check=check)

    def read(self, envelope, selection, grant_id=None, as_name=None, check=False):
        grant_id = grant_id or envelope['control']['attached_context']['grant_id']
        return self.run_bound(envelope, ['context', 'read'], {'grant_id': grant_id, 'selection': selection},
                              as_name=as_name, check=check)

    def bound_reads(self, envelope):
        return [{'input': op['input'], 'response': self.run_bound(envelope, op['argv'], op['input'], check=False)}
                for op in self.operations(envelope, 'context.read')]

    def send(self, tag, checkout=None, capture=True, evidence=None, request_id=None, source=None):
        evidence = evidence or [{'kind': 'diff', 'locator': REVIEWED}]
        context = {'capture_git_state': capture, 'snapshot': {
            'objective': f'Review {tag}', 'applicable_user_instructions': [], 'decisions': [],
            'completed_work': ['Edited the fixture'], 'current_work': ['Awaiting review'], 'blockers': [],
            'evidence': evidence}}
        if checkout is not None:
            context['checkout'] = {'path': str(checkout)}
        return {'request_id': request_id or f'lane-{tag}', 'author_kind': 'agent_session',
                'source': source or f'@reviewer Review {tag}; context from @worker.', 'context': context}

    def deliver(self, payload, recipient='reviewer'):
        sent = self.namedrop('worker', 'send', payload=payload)
        return sent, self.envelope(recipient, sent['message_id'])

    def deliver_to_idle_worker(self, message_id):
        self.report('worker', 'idle')
        receipt = self.envelope('worker', message_id)
        self.report('worker', 'working')
        return receipt

    def reply_and_settle(self, envelope, text='@worker Reviewed the pinned diff.'):
        reply = self.action(envelope, 'reply', source=text)['data']
        receipt = self.deliver_to_idle_worker(reply['message_id'])
        self.action(receipt, 'complete.no-reply')
        return reply

    def results(self, message_id):
        return self.namedrop('worker', 'handoff', 'results', payload={'message_id': message_id})

    def freshness(self, message_id):
        recipient = self.results(message_id)['recipients'][0]
        return {'result_status': recipient.get('result_status'), 'freshness': recipient.get('freshness'),
                'reviewed_checkout': recipient.get('reviewed_checkout')}

    def pane_text(self, pane, lines=400):
        raw = self.command(self.herdr, 'pane', 'read', pane, '--source', 'recent-unwrapped', '--lines', lines,
                           '--format', 'text')
        try:
            value = json.loads(raw)
        except ValueError:
            return raw
        result = value.get('result', value)
        return result.get('text') or result.get('content') or json.dumps(result, indent=2)

    def capture_panes(self, label):
        for name, pane in dict(self.panes, console=self.console_pane).items():
            self.save(f'panes/{label}-{name}.txt', self.pane_text(pane))

    def console(self, label, name, *argv, payload=None):
        self.console_runs += 1
        sentinel = f'lane-console-{self.console_runs}-done'
        if payload is not None:
            argv = (*argv, '--input-json', json.dumps(payload))
        command = ['env', 'HERDR_ENV=1', f'HERDR_SOCKET_PATH={self.env["HERDR_SOCKET_PATH"]}',
                   f'HERDR_WORKSPACE_ID={self.workspace}', f'HERDR_PANE_ID={self.panes[name]}',
                   f'HERDR_PLUGIN_STATE_DIR={self.state}', str(self.launcher), '--plugin-state', str(self.state), *argv]
        script = self.base/f'console-{self.console_runs}.sh'
        script.write_text('clear\nprintf \'%s\\n\' ' + shlex.quote(f'$ [{name}] namedrop {" ".join(argv[:2])}  # {label}')
                          + '\n' + shlex.join(command) + '\nprintf \'\\n%s\\n\' ' + sentinel + '\n')
        self.host('pane', 'run', self.console_pane, shlex.join(['sh', str(script)]))
        text = wait(lambda: (lambda t: t if f'\n{sentinel}' in t else None)(self.pane_text(self.console_pane)),
                    f'console output for {label}')
        self.save(f'panes/console-{self.console_runs:02d}-{label}.txt', text)
        return text

    def keep_envelopes(self):
        for name, inbox in self.inboxes.items():
            if inbox.exists():
                self.save(f'envelopes-{name}.jsonl', inbox.read_text())


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def canonical(path):
    return os.path.realpath(path)


def content(response):
    return response.get('data', {}).get('content') if response.get('status') == 'ok' else None


def refusal(response):
    return None if response.get('status') == 'ok' else (response.get('error') or {}).get('message', json.dumps(response))


class Run:

    def __init__(self, args, herdr):
        self.args, self.herdr = args, herdr
        self.evidence = args.evidence_dir
        self.checks, self.observations, self.cleanup = [], {}, {}

    def check(self, name, condition, detail=None):
        self.checks.append({'check': name, 'pass': bool(condition), 'detail': detail})
        if not condition:
            raise Failed(f'{name}: {json.dumps(detail, default=str)[:4000]}')

    def observe(self, key, value):
        self.observations[key] = value
        return value

    @contextmanager
    def fixture(self, side, binary, agents=('worker', 'reviewer')):
        fx = Lane(side, binary, self.herdr, self.evidence/side, agents)
        ok = False
        try:
            fx.start()
            self.observe(f'{side}.fixture', {'scope': str(fx.base), 'workspace': fx.workspace, 'panes': fx.panes,
                                             'console_pane': fx.console_pane, 'sessions': fx.sessions,
                                             'repo_a': str(fx.repo_a), 'repo_b': str(fx.repo_b),
                                             'cwd': {k: str(v) for k, v in fx.cwd.items()}})
            yield fx
            ok = True
        finally:
            try:
                fx.capture_panes('final')
            except Exception as error:
                fx.save('panes/final-capture-error.txt', repr(error))
            fx.keep_envelopes()
            status = fx.close()
            self.cleanup[side] = dict(status, retained=not (ok and status['verified']))
            if ok and status['verified']:
                shutil.rmtree(fx.base)


def default_round(run, fx, side, checkout=None, tag='default'):
    payload = fx.send(tag, checkout=checkout)
    sent, envelope = fx.deliver(payload)
    reads = fx.bound_reads(envelope)
    snapshot = next(content(r['response']) for r in reads if r['input']['selection']['kind'] == 'snapshot')
    pinned = json.dumps([content(r['response']) for r in reads if r['input']['selection']['kind'] == 'git_state'])
    fx.reply_and_settle(envelope)
    fresh = fx.freshness(sent['message_id'])
    fx.console(f'{tag}-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})
    return run.observe(f'{side}.{tag}', {'message_id': sent['message_id'], 'snapshot_checkout': snapshot.get('checkout'),
                                         'snapshot_git_state': snapshot.get('git_state'), 'pinned_reads': pinned,
                                         'results': fresh})


def default_and_explicit(run):
    sides = ([('baseline', run.args.baseline_binary)] if run.args.baseline_binary else []) + [('head', run.args.binary)]
    for side, binary in sides:
        with run.fixture(side, binary) as fx:
            before = fx.digests()
            run.check(f'{side}: worker CLI runs from checkout B', fx.cwd['worker'] == fx.repo_b)
            seen = default_round(run, fx, side)
            run.check(f'{side}: omitted selector pins the space checkout A despite caller cwd B',
                      A_SIDE in seen['pinned_reads'] and B_SIDE not in seen['pinned_reads'], seen['pinned_reads'])
            run.check(f'{side}: default review is fresh', seen['results']['freshness']['status'] == 'fresh', seen['results'])
            if side == 'baseline':
                attempt = fx.call('worker', 'send', payload=fx.send('explicit', checkout=fx.repo_b))
                run.observe('baseline.explicit_attempt', attempt)
                run.check('baseline: trunk has no checkout selector', 'checkout' in (refusal(attempt) or ''), attempt)
            else:
                run.check('head: default snapshot names the space checkout',
                          seen['snapshot_checkout'] == {'state': 'bound', 'selection': 'space_default',
                                                        'root': canonical(fx.repo_a), 'label': 'repo-a', 'layout': 'main'},
                          seen['snapshot_checkout'])
                explicit = default_round(run, fx, side, checkout=fx.repo_b, tag='explicit')
                run.check('head: explicit B pin holds captured B content',
                          B_SIDE in explicit['pinned_reads'] and A_SIDE not in explicit['pinned_reads'], explicit['pinned_reads'])
                run.check('head: explicit snapshot names checkout B',
                          explicit['snapshot_checkout'] == {'state': 'bound', 'selection': 'explicit',
                                                            'root': canonical(fx.repo_b), 'label': 'repo-b', 'layout': 'linked'},
                          explicit['snapshot_checkout'])
                run.check('head: explicit B review is fresh against B',
                          explicit['results']['freshness']['status'] == 'fresh'
                          and explicit['results']['reviewed_checkout']['root'] == canonical(fx.repo_b), explicit['results'])
            after = fx.digests()
            run.observe(f'{side}.digests', {'before': before, 'after': after})
            run.check(f'{side}: capture leaves A and B HEAD, index and status unchanged',
                      {k: v for k, v in after.items() if k != 'pins'} == {k: v for k, v in before.items() if k != 'pins'},
                      {'before': before, 'after': after})


def dirty_pin(run):
    with run.fixture('head', run.args.binary) as fx:
        b = fx.repo_b
        write(b/'src/staged.txt', 'B staged change\n')
        fx.git('add', 'src/staged.txt', repo=b)
        write(b/'src/new-untracked.txt', 'B untracked work\n')
        write(b/'.gitignore', '*.log\n')
        write(b/'src/ignored.log', 'B ignored log\n')
        before = fx.digests()
        paths = [REVIEWED, 'src/staged.txt', 'src/new-untracked.txt', 'src/ignored.log']
        sent, envelope = fx.deliver(fx.send('dirty', checkout=b, evidence=[{'kind': 'diff', 'locator': p} for p in paths]))
        after_capture = fx.digests()
        run.observe('digests', {'before': before, 'after_capture': after_capture})
        run.check('capture leaves A and B HEAD, index and status unchanged',
                  {k: after_capture[k] for k in 'AB'} == {k: before[k] for k in 'AB'}, run.observations['digests'])
        run.check('capture writes exactly one pin', len(after_capture['pins']) == len(before['pins']) + 1, after_capture['pins'])

        def snapshot_and_pages():
            snap = content(fx.read(envelope, {'kind': 'snapshot'}))
            stat = content(fx.read(envelope, {'kind': 'git_state', 'stat': True, 'start_line': 1, 'max_lines': 200}))
            pages = {p: fx.read(envelope, {'kind': 'git_state', 'path': p, 'start_line': 1, 'max_lines': 200}) for p in paths}
            return {'git_state': snap['git_state'], 'checkout': snap['checkout'], 'stat': stat,
                    'pages': {p: content(r) if content(r) is not None else {'refused': refusal(r)} for p, r in pages.items()}}
        captured = run.observe('captured', snapshot_and_pages())
        run.check('git_state records B branch and dirty work', captured['git_state'].get('branch') == 'lane-b'
                  and captured['git_state'].get('dirty') is True, captured['git_state'])
        run.check('pin holds the tracked modification', B_SIDE in json.dumps(captured['pages'][REVIEWED]), captured['pages'])
        run.check('pin holds the staged change', 'B staged change' in json.dumps(captured['pages']['src/staged.txt']), captured['pages'])
        run.check('pin holds the untracked nonignored file', 'B untracked work' in json.dumps(captured['pages']['src/new-untracked.txt']),
                  captured['pages'])
        run.check('pin leaves the ignored file out', 'ignored.log' not in json.dumps(captured['stat'])
                  and 'B ignored log' not in json.dumps(captured['pages']['src/ignored.log']), captured)
        write(b/REVIEWED, 'B changed after capture\n')
        fx.git('reset', '--quiet', 'src/staged.txt', repo=b)
        write(b/'src/staged.txt', 'B unstaged later\n')
        (b/'src/new-untracked.txt').unlink()
        reread = run.observe('after_b_changes', snapshot_and_pages())
        run.check('rereading the pin after B changes returns the captured work unchanged', reread == captured,
                  {'captured': captured, 'reread': reread})
        run.check('A is untouched', fx.digest(fx.repo_a) == before['A'] and (fx.repo_a/REVIEWED).read_text() == A_SIDE + '\n')
        fx.reply_and_settle(envelope)
        run.observe('results_after_b_changes', fx.freshness(sent['message_id']))
        fx.console('dirty-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})


def read_scope(run):
    with run.fixture('head', run.args.binary) as fx:
        a, b, outside = fx.repo_a, fx.repo_b, fx.base/'outside-secret.txt'
        write(outside, 'OUTSIDE-SECRET\n')
        write(b/'src/history.txt', 'B committed history\n')
        fx.git('add', 'src/history.txt', repo=b)
        fx.git('commit', '--quiet', '-m', 'B history', repo=b)
        commit = fx.git('rev-parse', 'HEAD', repo=b)
        write(b/'src/long.txt', ''.join(f'B long line {i}\n' for i in range(1, 451)))
        write(a/'src/link.txt', 'A ordinary link file\n')
        (b/'src/link.txt').symlink_to(outside)
        write(b/'src/notes.txt', 'B ordinary notes\n')
        (a/'src/notes.txt').symlink_to(outside)
        write(a/'.env', 'TOKEN=lane-secret-A\n')
        write(b/'.env', 'TOKEN=lane-secret-B\n')
        evidence = [{'kind': 'file', 'locator': REVIEWED}, {'kind': 'diff', 'locator': REVIEWED},
                    {'kind': 'file', 'locator': 'src/link.txt'}, {'kind': 'file', 'locator': 'src/notes.txt'},
                    {'kind': 'file', 'locator': '.env'}, {'kind': 'commit', 'locator': f'{commit}:src/history.txt'},
                    {'kind': 'diff', 'locator': 'src/long.txt'}, {'kind': 'file', 'locator': 'src/long.txt'}]
        run.check('reviewer runs from A', fx.cwd['reviewer'] == a)
        for label, root, mine, theirs in (('linked-B', b, 'B', 'A'), ('main-A', a, 'A', 'B')):
            sent, envelope = fx.deliver(fx.send(label, checkout=root, evidence=evidence))
            inspect = fx.action(envelope, 'context.inspect')['data']
            reads = {
                'snapshot': fx.read(envelope, {'kind': 'snapshot'}),
                'file_shared': fx.read(envelope, {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20}),
                'diff_shared': fx.read(envelope, {'kind': 'diff', 'locator': REVIEWED, 'max_lines': 200}),
                'file_link': fx.read(envelope, {'kind': 'file', 'locator': 'src/link.txt', 'start_line': 1, 'max_lines': 20}),
                'file_notes': fx.read(envelope, {'kind': 'file', 'locator': 'src/notes.txt', 'start_line': 1, 'max_lines': 20}),
                'file_env': fx.read(envelope, {'kind': 'file', 'locator': '.env', 'start_line': 1, 'max_lines': 20}),
                'commit': fx.read(envelope, {'kind': 'commit', 'locator': f'{commit}:src/history.txt', 'max_lines': 200}),
                'file_unlisted': fx.read(envelope, {'kind': 'file', 'locator': 'src/other.txt', 'start_line': 1, 'max_lines': 20}),
                'page_oversized': fx.read(envelope, {'kind': 'git_state', 'path': 'src/long.txt', 'start_line': 1, 'max_lines': 201}),
            }
            for start in (1, 201, 401):
                reads[f'pinned_long_{start}'] = fx.read(envelope, {'kind': 'git_state', 'path': 'src/long.txt',
                                                                   'start_line': start, 'max_lines': 200})
            reads['file_long_201'] = fx.read(envelope, {'kind': 'file', 'locator': 'src/long.txt', 'start_line': 201, 'max_lines': 200})
            shown = {k: content(r) if content(r) is not None else {'refused': refusal(r)} for k, r in reads.items()}
            run.observe(label, {'message_id': sent['message_id'], 'inspect': inspect, 'reads': shown})
            blocked = [entry.get('blocked') for entry in inspect['evidence']]
            run.check(f'{label}: inspect names the selected checkout', inspect['checkout']['root'] == canonical(root), inspect['checkout'])
            run.check(f'{label}: live file and diff reads use {mine}',
                      f'{mine}-SIDE' in json.dumps(shown['file_shared']) and f'{theirs}-SIDE' not in json.dumps(shown['file_shared'])
                      and f'{mine}-SIDE' in json.dumps(shown['diff_shared']), shown)
            escaping, ordinary = ('file_link', 'file_notes') if mine == 'B' else ('file_notes', 'file_link')
            run.check(f'{label}: the path that escapes {mine} is refused and inspect blocks it',
                      'refused' in shown[escaping] and 'OUTSIDE-SECRET' not in json.dumps(shown)
                      and blocked[[e['locator'] for e in evidence].index(f'src/{escaping[5:]}.txt')] is True, {'shown': shown, 'blocked': blocked})
            run.check(f'{label}: the path ordinary in {mine} reads its own content',
                      f'{mine} ordinary' in json.dumps(shown[ordinary]), shown[ordinary])
            run.check(f'{label}: protected .env is refused', 'refused' in shown['file_env'] and 'lane-secret' not in json.dumps(shown)
                      and blocked[4] is True, shown['file_env'])
            run.check(f'{label}: explicit commit read shows the B commit', 'B committed history' in json.dumps(shown['commit']), shown['commit'])
            run.check(f'{label}: unlisted path is refused', 'refused' in shown['file_unlisted'], shown['file_unlisted'])
            run.check(f'{label}: a page above 200 lines is refused', 'refused' in shown['page_oversized'], shown['page_oversized'])
            if mine == 'B':
                pages = [shown[f'pinned_long_{s}'] for s in (1, 201, 401)]
                numbers = [int(m) for p in pages if isinstance(p, str) for m in re.findall(r'^\+B long line (\d+)$', p, re.M)]
                run.check('linked-B: pinned pages are bounded and together cover the file once, in order',
                          all(isinstance(p, str) and len(p.splitlines()) <= 200 for p in pages)
                          and numbers == list(range(1, 451)), pages)
                lines = shown['file_long_201'].splitlines() if isinstance(shown['file_long_201'], str) else []
                run.check('linked-B: live file page starts at line 201 and stops at 200 lines',
                          lines[:1] == ['B long line 201'] and len(lines) == 200, shown['file_long_201'])
            fx.console(f'{label}-inspect', 'reviewer', 'context', 'inspect', payload=fx.operations(envelope, 'context.inspect')[0]['input'])
            fx.reply_and_settle(envelope)


def review_freshness(run):
    with run.fixture('head', run.args.binary) as fx:
        a, b = fx.repo_a, fx.repo_b
        sent, envelope = fx.deliver(fx.send('freshness', checkout=b))
        fx.bound_reads(envelope)
        fx.reply_and_settle(envelope)
        message = sent['message_id']
        sequence = []

        def step(label):
            seen = fx.freshness(message)
            sequence.append({'step': label, **seen})
            fx.console(label, 'worker', 'handoff', 'results', payload={'message_id': message})
            return seen['freshness']['status']
        run.check('fresh after the review', step('after-review') == 'fresh', sequence)
        write(b/REVIEWED, 'B edited after review\n')
        run.check('stale after the indexed B path changes', step('b-indexed-edit') == 'stale', sequence)
        write(b/REVIEWED, B_SIDE + '\n')
        run.check('fresh again once the content is restored', step('b-restored') == 'fresh', sequence)
        (b/REVIEWED).chmod(0o755)
        run.check('stale on an executable-bit change', step('b-mode-change') == 'stale', sequence)
        (b/REVIEWED).chmod(0o644)
        write(a/REVIEWED, 'A edited later\n')
        write(a/'src/other.txt', 'A unrelated\n')
        write(b/'src/other.txt', 'B unrelated\n')
        write(b/'src/unrelated-new.txt', 'B unrelated new file\n')
        run.check('unrelated A and B edits stay fresh', step('unrelated-a-b-edits') == 'fresh', sequence)
        run.observe('sequence', sequence)
        run.check('freshness names checkout B throughout',
                  all(s['reviewed_checkout']['root'] == canonical(b) for s in sequence), sequence)


def checkout_missing(run):
    with run.fixture('head', run.args.binary) as fx:
        a, b = fx.repo_a, fx.repo_b
        moved = fx.base/'repo-b-moved'
        evidence = [{'kind': 'diff', 'locator': REVIEWED}, {'kind': 'file', 'locator': REVIEWED}]
        sent, envelope = fx.deliver(fx.send('missing', checkout=b, evidence=evidence))
        fx.bound_reads(envelope)
        fx.reply_and_settle(envelope)
        run.check('fresh before B goes away', fx.freshness(sent['message_id'])['freshness']['status'] == 'fresh')
        fx.cwd['worker'] = a
        for label, change in (('moved', lambda: fx.git('worktree', 'move', b, moved)),
                              ('removed', lambda: fx.git('worktree', 'remove', '--force', moved))):
            change()
            seen = fx.freshness(sent['message_id'])
            reads = {'snapshot': fx.read(envelope, {'kind': 'snapshot'}),
                     'file': fx.read(envelope, {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20}),
                     'diff': fx.read(envelope, {'kind': 'diff', 'locator': REVIEWED, 'max_lines': 200}),
                     'git_state': fx.read(envelope, {'kind': 'git_state', 'path': REVIEWED, 'start_line': 1, 'max_lines': 200})}
            inspect = fx.action(envelope, 'context.inspect', check=False)
            shown = {k: content(r) if content(r) is not None else {'refused': refusal(r)} for k, r in reads.items()}
            run.observe(label, {'results': seen, 'reads': shown, 'inspect': inspect})
            run.check(f'{label}: freshness is unknown with checkout_unavailable',
                      seen['freshness']['status'] == 'unknown' and seen['freshness'].get('reason') == 'checkout_unavailable'
                      and seen['freshness'].get('checkout_reason') == 'root_missing', seen)
            run.check(f'{label}: file, diff and git_state evidence is refused',
                      all('refused' in shown[k] for k in ('file', 'diff', 'git_state')), shown)
            run.check(f'{label}: no read falls back to the same-named file in A',
                      A_SIDE not in json.dumps(shown) and A_SIDE not in json.dumps(inspect), shown)
            snap = shown['snapshot']
            run.check(f'{label}: snapshot prose stays readable with the unavailable checkout named',
                      isinstance(snap, dict) and snap.get('objective') == 'Review missing'
                      and snap['checkout'].get('state') == 'unavailable' and snap['checkout'].get('root') == canonical(b), snap)
            entries = (inspect.get('data') or {}).get('evidence', [])
            run.check(f'{label}: inspect marks entries unavailable and withholds their locators',
                      inspect.get('status') == 'ok' and entries
                      and all(e.get('available') is False and e.get('blocked') is True
                              and e.get('locator') == '[BLOCKED PATH]' for e in entries), inspect)
            fx.console(f'{label}-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})


def checkout_replaced(run):
    with run.fixture('head', run.args.binary) as fx:
        a, b, base = fx.repo_a, fx.repo_b, fx.base
        fx.cwd['worker'] = a

        def round_then(label, root, replace):
            evidence = [{'kind': 'diff', 'locator': REVIEWED}, {'kind': 'file', 'locator': REVIEWED}]
            sent, envelope = fx.deliver(fx.send(label, checkout=root, evidence=evidence))
            fx.bound_reads(envelope)
            fx.reply_and_settle(envelope)
            before = fx.freshness(sent['message_id'])
            run.check(f'{label}: fresh before replacement', before['freshness']['status'] == 'fresh', before)
            detail = replace()
            after = fx.freshness(sent['message_id'])
            reads = {'file': fx.read(envelope, {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20}),
                     'git_state': fx.read(envelope, {'kind': 'git_state', 'path': REVIEWED, 'start_line': 1, 'max_lines': 200})}
            shown = {k: content(r) if content(r) is not None else {'refused': refusal(r)} for k, r in reads.items()}
            live = (root/REVIEWED).read_text()
            run.observe(label, {'before': before, 'after': after, 'reads': shown, 'replacement': detail,
                                'live_content_matches_capture': live in (A_SIDE + '\n', B_SIDE + '\n')})
            run.check(f'{label}: freshness is unknown with a replacement reason',
                      after['freshness']['status'] == 'unknown' and after['freshness'].get('reason') == 'checkout_unavailable',
                      after)
            run.check(f'{label}: evidence reads refuse the replacement as an unavailable checkout',
                      all('checkout is unavailable' in shown[k].get('refused', '') for k in shown), shown)
            fx.console(f'{label}-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})

        def re_add():
            fx.git('worktree', 'remove', '--force', b)
            fx.git('worktree', 'add', '--quiet', b, 'lane-b')
            write(b/REVIEWED, B_SIDE + '\n')
            admin = fx.quiet_git(b, 'rev-parse', '--path-format=absolute', '--git-dir')
            return {'admin_dir': admin, 'content_identical': (b/REVIEWED).read_text() == B_SIDE + '\n'}
        round_then('re-added-same-path', b, re_add)

        def copied_pointer():
            admin = fx.quiet_git(b, 'rev-parse', '--path-format=absolute', '--git-dir')
            admin_inode = os.stat(admin).st_ino
            old = base/'repo-b-original'
            b.rename(old)
            shutil.copytree(old, b, symlinks=True)
            return {'admin_dir': admin, 'admin_inode_unchanged': os.stat(admin).st_ino == admin_inode,
                    'pointer': (b/'.git').read_text().strip(), 'git_still_works': fx.quiet_git(b, 'rev-parse', '--show-toplevel')}
        round_then('copied-root-same-admin', b, copied_pointer)

        def main_root_replaced():
            old = base/'repo-a-original'
            a.rename(old)
            a.mkdir()
            (old/'.git').rename(a/'.git')
            for item in old.iterdir():
                if item.is_dir():
                    shutil.copytree(item, a/item.name, symlinks=True)
                else:
                    shutil.copy2(item, a/item.name)
            return {'git_dir_preserved': (a/'.git').is_dir(), 'toplevel': fx.quiet_git(a, 'rev-parse', '--show-toplevel')}
        round_then('main-root-replaced-git-preserved', a, main_root_replaced)
        checks = [run.observations[k]['after']['freshness'].get('checkout_reason')
                  for k in ('re-added-same-path', 'copied-root-same-admin', 'main-root-replaced-git-preserved')]
        run.observe('checkout_reasons', checks)


def selector_validation(run):
    with run.fixture('head', run.args.binary) as fx:
        a, b, base = fx.repo_a, fx.repo_b, fx.base
        alias = base/'alias-b'
        alias.symlink_to(b)
        (base/'plain').mkdir()
        shutil.copytree(b, base/'copy-b', symlinks=True)
        other = base/'other-repo'
        other.mkdir()
        fx.git('init', '--quiet', repo=other)
        write(other/'src/shared.txt', 'other repository\n')
        fx.git('add', '.', repo=other)
        fx.git('commit', '--quiet', '-m', 'other', repo=other)
        invalid = {'subdirectory': b/'src', 'ordinary-directory': base/'plain', 'unregistered-pointer-copy': base/'copy-b',
                   'other-repository': other, 'relative': 'repo-b', 'missing': base/'absent'}
        start = fx.counts()
        refusals = {}
        for label, path in invalid.items():
            preview = fx.call('worker', 'handoff', 'preview', payload={k: v for k, v in fx.send(label, checkout=path).items()
                                                                        if k in ('source', 'context')})
            send = fx.call('worker', 'send', payload=fx.send(label, checkout=path))
            refusals[label] = {'path': str(path), 'preview': refusal(preview), 'send': refusal(send)}
            run.check(f'{label}: preview and send are refused', preview.get('status') != 'ok' and send.get('status') != 'ok',
                      refusals[label])
        after_invalid = fx.counts()
        run.observe('refusals', refusals)
        run.observe('counts', {'start': start, 'after_invalid': after_invalid})
        run.check('invalid selectors add no snapshot, grant, message, delivery, pin or object',
                  after_invalid == start, run.observations['counts'])
        preview = fx.namedrop('worker', 'handoff', 'preview', payload={k: v for k, v in fx.send('alias', checkout=alias).items()
                                                                        if k in ('source', 'context')})
        fx.console('alias-preview', 'worker', 'handoff', 'preview',
                   payload={k: v for k, v in fx.send('alias', checkout=alias).items() if k in ('source', 'context')})
        sent, envelope = fx.deliver(fx.send('alias', checkout=alias, evidence=[{'kind': 'diff', 'locator': REVIEWED},
                                                                          {'kind': 'file', 'locator': REVIEWED}]))
        snap = content(fx.read(envelope, {'kind': 'snapshot'}))
        run.observe('alias', {'preview_checkout': preview['context']['snapshot']['checkout'], 'snapshot_checkout': snap['checkout']})
        run.check('the symlink alias binds the canonical checkout B',
                  snap['checkout']['root'] == canonical(b) and snap['checkout']['root'] != str(alias)
                  and preview['context']['snapshot']['checkout']['root'] == canonical(b), run.observations['alias'])
        fx.reply_and_settle(envelope)
        alias.unlink()
        alias.symlink_to(a)
        reads = {'file': content(fx.read(envelope, {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20})),
                 'git_state': content(fx.read(envelope, {'kind': 'git_state', 'path': REVIEWED, 'start_line': 1, 'max_lines': 200})),
                 'results': fx.freshness(sent['message_id'])}
        run.observe('after_retarget', reads)
        run.check('retargeting the alias to A does not redirect saved evidence',
                  B_SIDE in json.dumps(reads['file']) and B_SIDE in json.dumps(reads['git_state'])
                  and A_SIDE not in json.dumps(reads) and reads['results']['freshness']['status'] == 'fresh'
                  and reads['results']['reviewed_checkout']['root'] == canonical(b), reads)
        fx.console('after-retarget-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})


def restart_session(fx, name, session):
    fx.host('pane', 'report-agent-session', fx.panes[name], '--source', 'herdr:codex', '--agent', 'codex',
            '--agent-session-id', session, '--session-start-source', 'startup')
    fx.sessions[name] = session
    wait(lambda: any(p['pane_id'] == fx.panes[name] and (p.get('agent_session') or {}).get('value') == session
                     for p in fx.live()), f'restarted {name} session reported')


def grant_flow(run, fx, side, checkout):
    file_sel = {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20}
    pin_sel = {'kind': 'git_state', 'path': REVIEWED, 'start_line': 1, 'max_lines': 200}
    evidence = [{'kind': 'diff', 'locator': REVIEWED}, {'kind': 'file', 'locator': REVIEWED}]
    marker = B_SIDE if checkout else A_SIDE
    attempts = {}

    def attempt(label, response):
        attempts[label] = {'status': response.get('status'), 'refusal': refusal(response),
                           'evidence_disclosed': marker in json.dumps(response)}
        return attempts[label]
    sent, first = fx.deliver(fx.send('grant-one', checkout=checkout, evidence=evidence))
    grant_one = first['control']['attached_context']['grant_id']
    attempt('reviewer reads its grant', fx.read(first, pin_sel))
    attempt('intruder runs the reviewer envelope read', fx.read(first, pin_sel, as_name='intruder'))
    attempt('intruder runs the reviewer envelope file read', fx.read(first, file_sel, as_name='intruder'))
    attempt('intruder reads the grant with its own CLI',
            fx.call('intruder', 'context', 'read', payload={'grant_id': grant_one, 'selection': pin_sel}))
    attempt('intruder inspects the reviewer thread',
            fx.call('intruder', 'context', 'inspect', payload=fx.operations(first, 'context.inspect')[0]['input']))
    attempt('worker CLI revokes an active grant', fx.call('worker', 'context', 'revoke', payload={'grant_id': grant_one}))
    request = fx.action(first, 'reply', source='@worker Please share your current context.', request_context=True)['data']
    revoked = request['context_request']['grant_id']
    receipt = fx.deliver_to_idle_worker(request['message_id'])
    decline = {k: v for k, v in fx.operations(receipt, 'reply')[0]['input'].items() if k != 'context'}
    declined = fx.run_bound(receipt, ['reply'], dict(decline, source='@reviewer Declining the context request.',
                                                     decline_context_request=revoked))['data']
    attempt('reviewer reads a revoked grant', fx.read(first, pin_sel, grant_id=revoked))
    fx.action(fx.envelope('reviewer', declined['message_id']), 'complete.no-reply')
    restart_session(fx, 'reviewer', '00000099-2222-4222-8222-000000000099')
    attempt('restarted reviewer, not yet registered, runs the old envelope read', fx.read(first, pin_sel))
    attempt('restarted reviewer, not yet registered, reads with its own CLI',
            fx.call('reviewer', 'context', 'read', payload={'grant_id': grant_one, 'selection': file_sel}))
    run.observe(f'{side}.reviewer_reregistration', fx.call('reviewer', 'name', 'reviewer', '--type', 'agent').get('status'))
    sent_two, second = fx.deliver(fx.send('grant-two', checkout=checkout, request_id='lane-grant-two', evidence=evidence))
    grant_two = second['control']['attached_context']['grant_id']
    attempt('re-registered reviewer reads its new grant', fx.read(second, pin_sel))
    attempt('re-registered reviewer reads its new grant live file', fx.read(second, file_sel))
    attempt('re-registered reviewer reads the earlier grant to the same address', fx.read(second, pin_sel, grant_id=grant_one))
    attempt('re-registered reviewer reads the revoked grant', fx.read(second, pin_sel, grant_id=revoked))
    attempt('intruder reads the new grant', fx.read(second, pin_sel, as_name='intruder'))
    fx.console(f'{side}-new-grant-read', 'reviewer', 'context', 'read', payload={'grant_id': grant_two, 'selection': pin_sel})
    fx.action(second, 'complete.no-reply')
    restart_session(fx, 'worker', '00000098-2222-4222-8222-000000000098')
    run.observe(f'{side}.worker_reregistration', fx.call('worker', 'name', 'worker', '--type', 'agent').get('status'))
    attempt('reviewer reads a grant whose source session restarted', fx.read(second, pin_sel, grant_id=grant_two))
    attempt('reviewer reads the first grant after its source session restarted', fx.read(second, pin_sel, grant_id=grant_one))
    fx.console(f'{side}-source-restarted-read', 'reviewer', 'context', 'read', payload={'grant_id': grant_two, 'selection': pin_sel})
    run.observe(f'{side}.grants', {'first': grant_one, 'revoked': revoked, 'second': grant_two, 'sessions': dict(fx.sessions),
                                   'second_attempt_differs': second['control']['attempt_id'] != first['control']['attempt_id']})
    return run.observe(f'{side}.attempts', attempts)


AUTHORIZED = ('reviewer reads its grant', 're-registered reviewer reads its new grant',
              're-registered reviewer reads its new grant live file')


def grant_session(run):
    agents = ('worker', 'reviewer', 'intruder')
    if run.args.baseline_binary:
        with run.fixture('baseline', run.args.baseline_binary, agents=agents) as fx:
            trunk = grant_flow(run, fx, 'baseline', None)
    with run.fixture('head', run.args.binary, agents=agents) as fx:
        head = grant_flow(run, fx, 'head', fx.repo_b)
    for label, seen in head.items():
        if label in AUTHORIZED:
            run.check(f'head: {label}', seen['status'] == 'ok' and seen['evidence_disclosed'], seen)
        elif label == 're-registered reviewer reads the earlier grant to the same address':
            run.check(f'head: {label} behaves as on trunk', not run.args.baseline_binary
                      or seen['status'] == trunk[label]['status'], {'head': seen, 'trunk': run.args.baseline_binary and trunk[label]})
        else:
            run.check(f'head: {label} is refused', seen['status'] != 'ok' and not seen['evidence_disclosed'], seen)
    if run.args.baseline_binary:
        run.check('every head authorization decision matches trunk',
                  {k: v['status'] for k, v in head.items()} == {k: v['status'] for k, v in trunk.items()},
                  {'head': head, 'trunk': trunk})


FSMONITOR_HOOK = '''#!/bin/sh
printf '%s %s\\n' "$(date +%s%N)" "$*" >> {log}
'''


def protected_probe(run, fx, side, root, explicit):
    base = fx.base
    outside = base/'outside-secret.txt'
    write(outside, 'OUTSIDE-SECRET-VALUE\n')
    write(root/'src/history-secret.txt', 'api_key = ghp_abcdefghijklmnopqrstuvwxyz0123456789\n')
    fx.git('add', 'src/history-secret.txt', repo=root)
    fx.git('commit', '--quiet', '-m', 'add secret', repo=root)
    secret_commit = fx.git('rev-parse', 'HEAD', repo=root)
    (root/'src/history-secret.txt').unlink()
    fx.git('commit', '--quiet', '-am', 'remove secret', repo=root)
    write(root/'.gitattributes', '*.flt filter=marker\n')
    write(root/'src/data.flt', 'filtered base\n')
    fx.git('add', '.gitattributes', 'src/data.flt', repo=root)
    fx.git('commit', '--quiet', '-m', 'filtered file', repo=root)
    clean_log, monitor_log = base/'marker-clean.log', base/'marker-fsmonitor.log'
    clean, monitor = base/'clean-filter.sh', base/'fsmonitor.sh'
    clean.write_text('#!/bin/sh\nprintf \'%s\\n\' "$(date +%s%N) $PWD" >> ' + shlex.quote(str(clean_log)) + '\nexec cat\n')
    monitor.write_text(FSMONITOR_HOOK.format(log=shlex.quote(str(monitor_log))))
    for script in (clean, monitor):
        script.chmod(0o755)
    fx.quiet_git(fx.repo_a, 'config', 'filter.marker.clean', str(clean))
    fx.quiet_git(fx.repo_a, 'config', 'filter.marker.smudge', 'cat')
    fx.quiet_git(fx.repo_a, 'config', 'core.fsmonitor', str(monitor))
    write(root/'src/data.flt', 'filtered dirty\n')
    write(root/'.env', 'TOKEN=lane-protected-env\n')
    write(root/'id_rsa', '-----BEGIN OPENSSH PRIVATE KEY-----\nlane-private-key-body\n-----END OPENSSH PRIVATE KEY-----\n')
    (root/'src/escape.txt').symlink_to(outside)
    write(root/'src/big.txt', ('x' * 1023 + '\n') * 1536)
    write(root/REVIEWED, ('B' if root == fx.repo_b else 'A') + '-SIDE protected probe\n')

    def markers():
        return {'clean_filter': clean_log.read_text().splitlines() if clean_log.exists() else [],
                'fsmonitor': monitor_log.read_text().splitlines() if monitor_log.exists() else []}

    def phase(label, action):
        before = markers()
        value = action()
        after = markers()
        fired = {k: after[k][len(before[k]):] for k in after}
        phases[label] = fired
        return value
    phases = {}
    paths = ['.env', 'id_rsa', 'src/escape.txt', 'src/data.flt', 'src/big.txt', REVIEWED]
    evidence = [{'kind': 'diff', 'locator': p} for p in paths] + [
        {'kind': 'file', 'locator': p} for p in ('.env', 'id_rsa', 'src/escape.txt', 'src/big.txt')] + [
        {'kind': 'commit', 'locator': f'{secret_commit}:src/history-secret.txt'}]
    payload = fx.send(f'{side}-protected', checkout=root if explicit else None, evidence=evidence)
    sent, envelope = phase('capture', lambda: fx.deliver(payload))

    def reads():
        out = {'bound': fx.bound_reads(envelope)}
        for p in ('.env', 'id_rsa', 'src/escape.txt', 'src/big.txt'):
            out[f'file {p}'] = fx.read(envelope, {'kind': 'file', 'locator': p, 'start_line': 1, 'max_lines': 200})
            out[f'diff {p}'] = fx.read(envelope, {'kind': 'diff', 'locator': p, 'max_lines': 200})
        for p in paths:
            out[f'git_state {p}'] = fx.read(envelope, {'kind': 'git_state', 'path': p, 'start_line': 1, 'max_lines': 200})
        out['git_state stat'] = fx.read(envelope, {'kind': 'git_state', 'stat': True, 'start_line': 1, 'max_lines': 200})
        out['commit history'] = fx.read(envelope, {'kind': 'commit', 'locator': f'{secret_commit}:src/history-secret.txt',
                                                   'max_lines': 200})
        out['inspect'] = fx.action(envelope, 'context.inspect', check=False)
        return out
    observed = phase('reads', reads)
    fx.reply_and_settle(envelope)
    fresh = phase('freshness', lambda: fx.freshness(sent['message_id']))
    text = json.dumps(observed) + json.dumps(fresh)
    leaks = {secret: secret in text for secret in ('lane-protected-env', 'lane-private-key-body', 'OUTSIDE-SECRET-VALUE',
                                                    'ghp_abcdefghijklmnopqrstuvwxyz0123456789')}
    sizes = {k: len(json.dumps(v)) for k, v in observed.items() if k != 'bound'}
    big_lines = [len(c.splitlines()) for k in ('git_state src/big.txt', 'file src/big.txt', 'diff src/big.txt')
                 for c in [content(observed[k])] if isinstance(c, str)]
    result = run.observe(f'{side}.{"explicit-B" if explicit else "default-A"}', {
        'markers': phases, 'leaks': leaks, 'largest_response_bytes': max(sizes.values()), 'big_page_lines': big_lines,
        'results': fresh, 'reads': {k: (content(v) if content(v) is not None else {'refused': refusal(v)})
                                    for k, v in observed.items() if k != 'bound' and k != 'inspect'},
        'inspect_evidence': ((observed['inspect'].get('data') or {}).get('evidence'))})
    fx.console(f'{side}-protected-results', 'worker', 'handoff', 'results', payload={'message_id': sent['message_id']})
    return result


def protected_evidence(run):
    sides = ([('baseline', run.args.baseline_binary)] if run.args.baseline_binary else []) + [('head', run.args.binary)]
    for side, binary in sides:
        with run.fixture(side, binary) as fx:
            seen = protected_probe(run, fx, side, fx.repo_a, explicit=False)
            run.check(f'{side} default-A: no protected content appears', not any(seen['leaks'].values()), seen['leaks'])
            run.check(f'{side} default-A: freshness is unknown', seen['results']['freshness']['status'] == 'unknown', seen['results'])
            run.check(f'{side} default-A: bounded pages', all(n <= 200 for n in seen['big_page_lines'])
                      and seen['largest_response_bytes'] < 1_100_000, seen['big_page_lines'])
            if side == 'head':
                run.check('head default-A: fsmonitor never runs during capture, reads or freshness',
                          not any(p['fsmonitor'] for p in seen['markers'].values()), seen['markers'])
        if side == 'head':
            with run.fixture('head-explicit', binary) as fx:
                seen = protected_probe(run, fx, 'head', fx.repo_b, explicit=True)
                run.check('head explicit-B: no protected content appears', not any(seen['leaks'].values()), seen['leaks'])
                run.check('head explicit-B: freshness is unknown', seen['results']['freshness']['status'] == 'unknown', seen['results'])
                run.check('head explicit-B: bounded pages', all(n <= 200 for n in seen['big_page_lines'])
                          and seen['largest_response_bytes'] < 1_100_000, seen['big_page_lines'])
                run.check('head explicit-B: fsmonitor never runs during capture, reads or freshness',
                          not any(p['fsmonitor'] for p in seen['markers'].values()), seen['markers'])
                run.check('head explicit-B: no clean filter runs during reads or freshness',
                          not seen['markers']['reads']['clean_filter'] and not seen['markers']['freshness']['clean_filter'],
                          seen['markers'])
    run.observe('marker_summary', {k: v['markers'] for k, v in run.observations.items() if isinstance(v, dict) and 'markers' in v})


def envelope_lifecycle(run):
    with run.fixture('head', run.args.binary) as fx:
        b = fx.repo_b
        payload = fx.send('lifecycle', checkout=b)
        before = fx.counts()
        digests = fx.digests()
        preview = fx.namedrop('worker', 'handoff', 'preview', payload={k: payload[k] for k in ('source', 'context')})
        fx.console('preview', 'worker', 'handoff', 'preview', payload={k: payload[k] for k in ('source', 'context')})
        run.observe('preview', preview)
        run.check('preview resolves checkout B', preview['context']['snapshot']['checkout']['root'] == canonical(b), preview['context'])
        run.check('preview writes no row, pin or Git object and leaves A and B untouched',
                  fx.counts() == before and fx.digests() == digests, {'before': before, 'after': fx.counts()})
        sent, envelope = fx.deliver(payload)
        after_send = fx.counts()
        replay = fx.namedrop('worker', 'send', payload=payload)
        after_replay = fx.counts()
        rows = lambda counts: {k: v for k, v in counts.items() if k in STATE_TABLES}
        run.check('replaying the send returns the same message and publishes no snapshot, grant or delivery',
                  replay['message_id'] == sent['message_id'] and replay.get('idempotent_replay') is True
                  and rows(after_replay) == rows(after_send), {'after_send': after_send, 'after_replay': after_replay})
        orphan_pins = sorted(set(after_replay['pins']) - set(after_send['pins']))
        run.observe('replay_orphan_pins', orphan_pins)
        reads = fx.bound_reads(envelope)
        snapshot = next(content(r['response']) for r in reads if r['input']['selection']['kind'] == 'snapshot')
        pinned = json.dumps([content(r['response']) for r in reads if r['input']['selection']['kind'] == 'git_state'])
        inspect = fx.action(envelope, 'context.inspect')['data']
        run.observe('bound_reads', {'tokens': [r['input'] for r in reads], 'snapshot_checkout': snapshot['checkout'],
                                    'pinned': pinned, 'inspect_checkout': inspect['checkout']})
        run.check('bound reads use the selected snapshot in B',
                  snapshot['checkout']['root'] == canonical(b) and B_SIDE in pinned and A_SIDE not in pinned
                  and inspect['checkout']['root'] == canonical(b), run.observations['bound_reads'])
        request = fx.action(envelope, 'reply', source='@worker Please share your current context.', request_context=True)['data']
        receipt = fx.deliver_to_idle_worker(request['message_id'])
        run.check('the worker envelope carries the context request',
                  receipt['control'].get('context_request', {}).get('grant_id') == request['context_request']['grant_id'],
                  receipt['control'].get('context_request'))
        fulfil = fx.operations(receipt, 'reply')[0]
        body = dict(fulfil['input'], source='@reviewer Here is my checkout B context.')
        body['context'] = dict(body['context'], checkout={'path': str(b)}, capture_git_state=True,
                               snapshot=dict(body['context']['snapshot'], objective='Fulfil the review context request',
                                             evidence=[{'kind': 'diff', 'locator': REVIEWED}]))
        fulfilled = fx.run_bound(receipt, fulfil['argv'], body)['data']
        counts_fulfilled = fx.counts()
        replayed = fx.run_bound(receipt, fulfil['argv'], body)['data']
        counts_replayed = fx.counts()
        run.check('replaying the fulfilment reply returns the same message and publishes no snapshot or grant',
                  replayed['message_id'] == fulfilled['message_id'] and rows(counts_replayed) == rows(counts_fulfilled),
                  {'fulfilled': counts_fulfilled, 'replayed': counts_replayed})
        run.observe('fulfilment_replay_orphan_pins', sorted(set(counts_replayed['pins']) - set(counts_fulfilled['pins'])))
        delivered = fx.envelope('reviewer', fulfilled['message_id'])
        fulfilment_reads = fx.bound_reads(delivered)
        fulfilment_snapshot = next(content(r['response']) for r in fulfilment_reads if r['input']['selection']['kind'] == 'snapshot')
        fulfilment_pinned = json.dumps([content(r['response']) for r in fulfilment_reads
                                        if r['input']['selection']['kind'] == 'git_state'])
        run.observe('fulfilment', {'grant': delivered['control'].get('attached_context'),
                                   'snapshot_checkout': fulfilment_snapshot['checkout'], 'pinned': fulfilment_pinned})
        run.check('the fulfilment envelope binds the selected B snapshot',
                  delivered['control']['attached_context']['grant_id'] == request['context_request']['grant_id']
                  and fulfilment_snapshot['checkout']['root'] == canonical(b) and B_SIDE in fulfilment_pinned,
                  run.observations['fulfilment'])
        fx.action(delivered, 'complete.no-reply')
        captured = fx.freshness(sent['message_id'])
        off_sent, off_envelope = fx.deliver(fx.send('capture-off', checkout=b, capture=False, evidence=[
            {'kind': 'diff', 'locator': REVIEWED}, {'kind': 'file', 'locator': REVIEWED}]))
        off_snapshot = content(fx.read(off_envelope, {'kind': 'snapshot'}))
        off_file = content(fx.read(off_envelope, {'kind': 'file', 'locator': REVIEWED, 'start_line': 1, 'max_lines': 20}))
        fx.reply_and_settle(off_envelope)
        off = fx.freshness(off_sent['message_id'])
        run.observe('freshness', {'capture_on': captured, 'capture_off': off,
                                  'capture_off_snapshot': {'checkout': off_snapshot['checkout'], 'git_state': off_snapshot.get('git_state')},
                                  'capture_off_live_file': off_file})
        run.check('capture-on review is fresh against B', captured['freshness']['status'] == 'fresh', captured)
        run.check('capture-off snapshot is bound to B without a pin, its live file read uses B and freshness is unknown',
                  off_snapshot['checkout']['root'] == canonical(b) and off_snapshot.get('git_state') is None
                  and B_SIDE in json.dumps(off_file) and off['freshness']['status'] == 'unknown', run.observations['freshness'])
        fx.console('capture-off-results', 'worker', 'handoff', 'results', payload={'message_id': off_sent['message_id']})


SCENARIOS = {
    'default-and-explicit': default_and_explicit, 'dirty-pin': dirty_pin, 'read-scope': read_scope,
    'review-freshness': review_freshness, 'checkout-missing': checkout_missing, 'checkout-replaced': checkout_replaced,
    'selector-validation': selector_validation, 'grant-session': grant_session,
    'protected-evidence': protected_evidence, 'envelope-lifecycle': envelope_lifecycle,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--baseline-binary', type=Path)
    parser.add_argument('--scenario', choices=SCENARIOS, required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    args.baseline_binary = args.baseline_binary and args.baseline_binary.resolve()
    args.evidence_dir = args.evidence_dir.resolve()
    args.evidence_dir.mkdir(parents=True, exist_ok=False)
    herdr = shutil.which('herdr')
    assert herdr, 'Native Herdr is required'
    run = Run(args, herdr)
    binaries = {'head': args.binary, **({'baseline': args.baseline_binary} if args.baseline_binary else {})}
    harness = Path(__file__).resolve()
    report = {'scenario': args.scenario, 'invocation': [sys.executable, *sys.argv],
              'harness': {'path': str(harness), 'sha256': sha256(harness),
                          'git_sha': subprocess.run(['git', '-C', harness.parent, 'rev-parse', 'HEAD'], capture_output=True,
                                                    text=True).stdout.strip(),
                          'git_status': subprocess.run(['git', '-C', harness.parent, 'status', '--porcelain'],
                                                       capture_output=True, text=True).stdout},
              'host': {'platform': platform.platform(), 'python': sys.version.split()[0],
                       'herdr': subprocess.run([herdr, '--version'], capture_output=True, text=True).stdout.strip(),
                       'shell': os.environ.get('NAMEDROP_TEST_SHELL') or ('zsh' if shutil.which('zsh') else 'bash'),
                       'zsh_installed': bool(shutil.which('zsh'))},
              'binaries': {side: {'path': str(p), 'sha256': sha256(p)} for side, p in binaries.items()},
              'simulation': 'compiled cat plus OSC title renderer per agent pane; no model'}
    error = None
    try:
        SCENARIOS[args.scenario](run)
    except Exception as failure:
        error = failure
    cleanup_ok = bool(run.cleanup) and all(c['verified'] for c in run.cleanup.values())
    passed = error is None and cleanup_ok
    report.update({'pass': passed, 'error': None if error is None else f'{type(error).__name__}: {error}',
                   'checks': run.checks, 'observations': run.observations})
    (args.evidence_dir/'result.json').write_text(json.dumps(report, indent=2, default=str) + '\n')
    (args.evidence_dir/'cleanup_status.json').write_text(json.dumps(run.cleanup, indent=2) + '\n')
    print('Evidence:', args.evidence_dir)
    print(f'{args.scenario}: {"PASS" if passed else "FAIL"} ({sum(c["pass"] for c in run.checks)}/{len(run.checks)} checks, '
          f'cleanup {"verified" if cleanup_ok else "INCOMPLETE"})')
    if error is not None:
        raise error
    if not cleanup_ok:
        sys.exit(f'Cleanup incomplete: {run.cleanup}')


if __name__ == '__main__':
    main()
