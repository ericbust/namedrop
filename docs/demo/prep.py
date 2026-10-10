"""Crop captured pane screens into frames.js for the demo scene.

Each frame is real `herdr pane read --ansi` (or headless-client) output from
the isolated demo profile. We only choose rows, drop Claude Code's grey
prompt suggestions (text nobody typed), and shorten the demo HOME path.
"""
import json, re, sys
from pathlib import Path

CAP = Path(sys.argv[1] if len(sys.argv) > 1 else Path.home() / 'namedrop-demo' / 'capture')
SGR = re.compile(r'\x1b\[[0-9;]*m')
# the original capture's profile, then the current one
HOMES = ['/private/tmp/namedrop-demo/home', str(Path.home() / 'namedrop-demo' / 'home')]

# id: (file, first row, last row exclusive or None)
FRAMES = {
    # naming: Claude Code and Codex, typed and answered
    'name_rev_typed':     ('name-reviewer-typed.ansi', None, None),
    'name_rev_done':      ('name-reviewer-done.ansi', 0, 19),
    'name_coder_typed':   ('name-coder-typed.ansi', None, None),
    'name_coder_done':    ('name-coder-done.ansi', 0, 30),
    # act 1: one lead, two workers, one merged answer
    'lead_typed':         ('03-lead-typed.ansi', None, None),
    'lead_sent':          ('seq-team/1790259060-p6.ansi', 0, 14),
    'rev_work':           ('seq-team/1790259146-p7.ansi', None, None),
    'tst_work':           ('seq-team/1790259192-p8.ansi', None, None),
    'rev_work2':          ('seq-team/1790259201-p7.ansi', None, None),
    'tst_work2':          ('seq-team/1790259210-p8.ansi', None, None),
    'rev_reply':          ('seq-team/1790259250-p7.ansi', 25, 40),
    'tst_reply':          ('seq-team/1790259226-p8.ansi', 30, 41),
    'lead_answer':        ('seq-team/1790259315-p6.ansi', 10, 45),
    # peers: separate jobs for @coder (Codex) and @tester, no lead
    'peer_coder_typed':   ('peer-coder-typed.ansi', 21, None),
    'peer_coder_asks':    ('seq-peers/1790391467-p2.ansi', 28, 39),
    'peer_coder_extends': ('seq-peers/1790391563-p2.ansi', 21, 37),
    'peer_coder_rereview': ('seq-peers/1790391638-p2.ansi', 25, 39),
    'peer_rev_idle':      ('name-reviewer-done.ansi', 7, 19),
    'peer_rev_work':      ('seq-peers/1790391500-p3.ansi', 30, 40),
    'peer_rev_findings':  ('seq-peers/1790391546-p3.ansi', 24, 40),
    'peer_rev_stale':     ('seq-peers/1790391640-p3.ansi', 29, 40),
    'peer_rev_fresh':     ('seq-peers/1790391700-p3.ansi', 26, 40),
    'peer_tester_typed':  ('peer-tester-typed.ansi', 10, None),
    'peer_tester_run':    ('seq-peers/1790391399-p4.ansi', 29, 40),
    'peer_tester_tells':  ('seq-peers/1790391434-p4.ansi', 29, 40),
    'peer_tester_pass':   ('seq-peers/1790391498-p4.ansi', 30, 40),
    'peer_build_idle':    ('name-build-done.ansi', None, None),
    'peer_build_fail':    ('seq-peers/1790391403-p1.ansi', 21, 45),
    'peer_build_pass':    ('seq-peers/1790391484-p1.ansi', 30, 45),
    # Agent status from @tester's pane while it waits on @coder, then scrolled to the rest of the workspace
    'ui_status':          ('ui-status-final-2.ansi', 0, 46),
    'ui_status_scrolled': ('ui-status-final-scrolled.ansi', 0, 46),
}
PEER_ROWS = 16  # every peer window shows the same number of rows, so the grid lines up


def plain(line):
    return SGR.sub('', line)


def lines_of(name):
    text = (CAP / name).read_text()
    return [l.rstrip('\r') for l in text.replace('\r\n', '\n').split('\n')]


def drop_suggestion(line):
    # Claude Code draws an unsent prompt suggestion dimmed after the prompt.
    if plain(line).lstrip('│').startswith('❯\xa0') and line.split('❯', 1)[1][1:].startswith('\x1b[0m\x1b[2m'):
        return line.split('❯', 1)[0] + '❯\xa0\x1b[0m'
    return line


def shorten_home(line):
    for home in HOMES:
        line = line.replace(home, '~')
        # Codex cuts long lines short, which can leave a truncated prefix of the path
        line = re.sub('(?:' + '|'.join(re.escape(home[:n]) for n in range(len(home) - 1, 8, -1)) + ')…', '~…', line)
    return line


# pyte renders the popups' 16-colour text with xterm's stock palette; map it to
# the Catppuccin-style colours Herdr itself draws so it stays legible.
PALETTE = {'0;0;238': '137;180;250', '0;205;205': '125;226;209', '0;205;0': '166;227;161',
           '205;0;0': '243;139;168', '205;205;0': '249;226;175', '205;0;205': '203;166;247'}


def repaint(line):
    for old, new in PALETTE.items():
        line = re.sub(rf'([34]8;2;){old}(?=[;m])', rf'\g<1>{new}', line)
    return line


def tidy(rows):
    """Drop Claude Code's one-time notice and squeeze tall blank gaps to one row."""
    out = []
    for r in rows:
        text = plain(r)
        if text.startswith('▎ ') or text.strip() == '1 more notice hidden':
            continue
        if not text.strip() and out and not plain(out[-1]).strip():
            continue
        out.append(r)
    return out


def window(rows, keep_from, keep_to):
    """Trim blank rows, keeping the requested window."""
    rows = tidy(rows[keep_from:keep_to])
    while rows and not plain(rows[-1]).strip():
        rows.pop()
    while rows and not plain(rows[0]).strip():
        rows.pop(0)
    return rows


def last_block(rows, marker):
    """From the last row containing marker to the end."""
    idx = max(i for i, r in enumerate(rows) if marker in plain(r))
    return rows[idx:]


def starting_at(rows, marker, before=0):
    """From `before` rows above the first row containing marker."""
    return rows[max(0, next(i for i, r in enumerate(rows) if marker in plain(r)) - before):]


out = {}
for key, (name, a, b) in FRAMES.items():
    rows = [shorten_home(drop_suggestion(repaint(l))) for l in lines_of(name)]
    if key.startswith('ui_'):
        # The client capture has no dim attribute, so blank the focused pane's input box by text: it holds
        # the stray keys the headless terminal leaves when it sends the prefix.
        box = max(i for i, r in enumerate(rows) if '❯\xa0' in plain(r))
        rows[box] = re.sub('❯\xa0[^\x1b]*', lambda m: '❯\xa0' + ' ' * (len(m.group(0)) - 2), rows[box])
    if key == 'lead_typed':  # the lead's own naming reply sits above the prompt box
        rows = starting_at(rows, 'Ask @reviewer', before=1)
    elif key == 'lead_sent':
        rows = starting_at(rows, 'Ask @reviewer')
    if key in ('rev_work', 'tst_work', 'rev_work2', 'tst_work2'):  # below the delivered request
        rows = last_block(rows, 'untrusted_body')[2:]
    elif key.startswith('peer_build_'):
        rows = [r for r in rows[a:b] if plain(r).strip()][-PEER_ROWS:]
        a = b = None
    rows = window(rows, a, b)
    if key.startswith('peer_') and len(rows) > PEER_ROWS:
        sys.exit(f'{key}: {len(rows)} rows, more than the {PEER_ROWS} a peer window shows')
    cols = max(len(plain(r)) for r in rows)
    out[key] = {'rows': rows, 'cols': cols}
    print(f'{key:19} {len(rows):3} rows {cols:4} cols  {plain(rows[0])[:60]!r}', file=sys.stderr)

Path(__file__).with_name('frames.js').write_text('window.FRAMES = ' + json.dumps(out) + ';\n')
