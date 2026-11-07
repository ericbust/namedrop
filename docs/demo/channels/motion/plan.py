"""Plan the Channels highlight from a take, on the stage of the Namedrop demo.

The Channels window stays whole and still on the stage, with the real Codex and Claude Code
windows beside it. In each shot a spotlight ring marks the focal element, found as the tight
box of real ink in the source frame, and a callout repeats its key text in large type. Cards
quote real messages as they travel between Channels and the agents, and between agents. Every callout and card must
be text the page or an agent's screen really showed. The planner fails when a hold, a quote,
the focal text size or the total length breaks the storyboard rules."""
import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import shutil
import subprocess

import numpy as np
from PIL import Image, ImageFont

from ink import ink_mask, line_caps

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('directory', type=Path, help='take directory with recording/recording.json')
parser.add_argument('--shotlist', type=Path, help='also write a Markdown shot list here')
parser.add_argument('--font', type=Path, help='TrueType font for the callouts (default: Liberation Sans Bold)')
parser.add_argument('--rehearsal', action='store_true',
                    help='report the word "operator" as a warning, to plan a rehearsal take; never for a published take')
args = parser.parse_args()
recording = json.loads((args.directory / 'recording/recording.json').read_text())
events = {event['name']: event for event in recording['events']}
VIEW_W, VIEW_H = recording['viewport']['width'], recording['viewport']['height']
DPR = recording['deviceScaleFactor']

# The stage is 1920×1080 logical px, rendered at 4/3 to 2560×1440. World x runs right and
# y up from the centre; a window at z = 0 maps one world unit to one logical pixel.
W, H, BAR = 1920, 1080, 38
APP_SCALE = 1.04
APP_LEFT, APP_TOP = 36, 176
COLUMN = (1404, 1886)                       # the agent windows' column, in screen x
AGENT_TOPS = {'codex': 238, 'claude': 676}  # window top edges, in screen y; plates sit above
AGENT_ROWS = 21
README_WIDTH = 900
MIN_HOLD, FADE, SPOT_IN = 2.5, 0.45, 0.6
MIN_CAP_README = 9.5
CALLOUT_MAIN, CALLOUT_WHO, CALLOUT_PAD, CALLOUT_GAP = 34, 21, 22, 16
DURATION = (55, 75)
END_SECONDS = 3.6
APP = {'x': APP_LEFT, 'y': APP_TOP + BAR * APP_SCALE, 'scale': APP_SCALE}


def at(point):
    name, offset = point if isinstance(point, tuple) else (point, 0)
    return events[name]['t'] + offset


def nearest(items, t):
    return min(items, key=lambda item: abs(item['t'] - t))


def ink_box(rect, t, pad):
    """The padded bounding box, in CSS px, of the ink inside rect, and the smallest cap height
    of its text lines in CSS px, both read from the source frame."""
    image = Image.open(args.directory / 'recording' / nearest(recording['frames'], t)['file']).convert('L')
    x0, y0 = max(0, int(rect['x'] * DPR)), max(0, int(rect['y'] * DPR))
    x1 = min(image.width, int((rect['x'] + rect['width']) * DPR))
    y1 = min(image.height, int((rect['y'] + rect['height']) * DPR))
    mask = ink_mask(image.crop((x0, y0, x1, y1)))
    ys, xs = np.nonzero(mask)
    if not len(xs):
        raise SystemExit(f'no ink inside {rect} at {t:.2f}s')
    caps = line_caps(mask[ys.min():ys.max() + 1, xs.min():xs.max() + 1])
    box = {'x': (x0 + xs.min()) / DPR - pad, 'y': (y0 + ys.min()) / DPR - pad,
           'width': (xs.max() - xs.min()) / DPR + 2 * pad, 'height': (ys.max() - ys.min()) / DPR + 2 * pad}
    return box, min(caps) / DPR


BANNED = re.compile('operator', re.I)
SGR = re.compile(r'\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)')


def plain(line):
    return SGR.sub('', line)


def flatten(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for item in value.values():
            yield from flatten(item)
    elif isinstance(value, list):
        for item in value:
            yield from flatten(item)


def normalize(text):
    return re.sub(r'\s+', ' ', text).strip()


# What the take really showed: the page text and agent screens the steps returned, and the
# text of every measured focal element.
SHOWN = normalize(' '.join(
    [text for path in sorted((args.directory / 'actions').glob('*.result'))
     for text in flatten(json.loads(path.read_text()).get('value'))] +
    [event['focus']['read']['text'] for event in recording['events'] if 'focus' in event]))

# ---------- the agents' real screens ----------
HOME = recording['profileHome'].rstrip('/')
PRIVATE = HOME.rsplit('/profiles/', 1)[0]


def shorten_home(line):
    """The profile's home as ~, as docs/demo/prep.py does, including a path Codex cut short."""
    line = line.replace(HOME, '~')
    return re.sub('(?:' + '|'.join(re.escape(HOME[:n]) for n in range(len(HOME) - 1, 8, -1)) + ')…', '~…', line)


def drop_suggestion(line):
    """Claude Code draws an unsent prompt suggestion dimmed after its prompt; nobody typed it."""
    if plain(line).lstrip('│').startswith('❯\xa0') and line.split('❯', 1)[1][1:].startswith('\x1b[0m\x1b[2m'):
        return line.split('❯', 1)[0] + '❯\xa0\x1b[0m'
    return line


def screen_rows(file):
    """The rows a window shows: what follows the last delivered request, without its JSON
    envelope, the testbed's private paths, Claude Code's unsent suggestion or the word
    "operator", blank runs squeezed, the newest rows kept."""
    rows = [shorten_home(drop_suggestion(line.rstrip('\r'))) for line in (args.directory / 'recording' / file).read_text().split('\n')]
    marks = [i for i, row in enumerate(rows) if 'untrusted_body' in plain(row)]
    if marks:
        rows = rows[marks[-1] + 1:]
    kept = []
    for row in rows:
        text = plain(row)
        # Namedrop still binds replies to the alias @operator, so an agent may write it in its
        # pane. The person in the movie is "you": rows that say it are left out, never edited.
        if PRIVATE in text or re.search(r'[{}]"|"[{}:,]|"\]', text) or BANNED.search(text):
            continue
        if not text.strip() and (not kept or not plain(kept[-1]).strip()):
            continue
        kept.append(row)
    while kept and not plain(kept[-1]).strip():
        kept.pop()
    return kept[-AGENT_ROWS:]


FRAMES = {}


def pane_key(alias, t):
    """The key of the agent's screen at source time t; identical screens share one key."""
    capture = nearest(recording['panes'], t)
    rows = screen_rows(capture[alias])
    for key, frame in FRAMES.items():
        if key.startswith(alias + '-') and frame['rows'] == rows:
            return key
    key = f'{alias}-{capture[alias].split("/")[-1].split("-")[0]}'
    FRAMES[key] = {'rows': rows, 'cols': max([len(plain(row)) for row in rows] or [1]), 'sourceTime': capture['t']}
    return key


@dataclass
class Shot:
    name: str
    focus: str                # event whose measured element holds the focal text
    ink: object               # source time of the frame the ring is found in
    segments: list            # (output seconds, source from, source to); the last one is the hold
    callout: tuple            # (who, text): real on-screen text repeated beside the ring
    shown: object             # source time from which the callout text is on screen
    agent: str                # the agent this shot is about: its colour marks the callout
    cards: tuple = ()         # (sender, recipient, source time or seconds into the shot, real message text)
    search: dict = None       # search rectangle in CSS px instead of the event's element
    label: str = ''
    pad: float = 12
    plan: dict = field(default_factory=dict)


# The quoted replies, reminder bodies and activity lines are the take's own text; update them
# for a new take. plan.py refuses any that the take did not show.
TAKE_TEXT = {
    'name': 'Crescendo',
    'names': 'Momentum, Horizon',
    'asks': 'Hi both. @codex, please reply with a five-word release risk.',
    'risk': 'Unresolved dependencies may delay launch.',
    'pending': 'Tag the release.',
    'answer': 'Reminder: Tag the release.',
    'subagents': '1 subagent',
}

SCENES = [
    ('Ask an agent in a channel', 'codex', [
        Shot('ask', 'ask-typed', ('ask-typed', 0.2),
             [(1.6, ('ask-wide', 1.4), 'ask-typed'), (2.6, 'ask-typed', ('ask-typed', 1.75))],
             ('', '@codex Suggest a name for the 1.5 release. One word.'), 'ask-typed', 'codex'),
        Shot('reply', 'ask-reply', ('ask-reply', 0.5),
             [(0.8, ('ask-sent', 0.2), ('ask-reply', -0.2)), (2.8, ('ask-reply', -0.2), ('ask-reply', 2.6))],
             ('@codex', TAKE_TEXT['name']), 'ask-reply', 'codex',
             cards=(('you', 'codex', ('ask-sent', 0.2), 'Suggest a name for the 1.5 release. One word.'),
                    ('codex', 'you', 'ask-reply', TAKE_TEXT['name']))),
    ]),
    ('Follow up in the thread. No @ needed.', 'codex', [
        Shot('follow-up', 'followup-typed', ('followup-typed', 0.2),
             [(1.0, ('followup-open', -0.1), 'followup-typed'), (2.6, 'followup-typed', ('followup-sent', -0.05))],
             ('To @codex', 'Two more, please.'), 'followup-typed', 'codex'),
        Shot('thread reply', 'followup-reply', ('followup-reply', 0.5),
             [(0.8, ('followup-sent', 0.2), ('followup-reply', -0.2)), (2.8, ('followup-reply', -0.2), ('followup-reply', 2.6))],
             ('@codex', TAKE_TEXT['names']), 'followup-reply', 'codex',
             cards=(('you', 'codex', ('followup-sent', 0.2), 'Two more, please.'),
                    ('codex', 'you', 'followup-reply', TAKE_TEXT['names']))),
    ]),
    ('Agents work together in the open', 'both', [
        Shot('collab ask', 'collab-typed', ('collab-typed', 0.2),
             [(1.6, ('collab-open', 0.4), 'collab-typed'), (2.6, 'collab-typed', ('collab-typed', 1.75))],
             ('', '@claude Reply to me and @codex, and ask @codex for a five-word release risk.'), 'collab-typed', 'claude'),
        # All activity says who each agent is working for: Codex for Claude, then Claude for Codex.
        Shot('codex works', 'activity-ask', ('activity-ask', 0.6),
             [(0.6, ('collab-sent', 0.2), ('activity-ask', 0.2)), (3.0, ('activity-ask', 0.2), ('activity-ask', 3.2))],
             ('@codex', TAKE_TEXT['asks']), ('activity-ask', 0.2), 'codex',
             cards=(('you', 'claude', ('collab-sent', 0.2), 'Reply to me and @codex, and ask @codex for a five-word release risk.'),
                    ('claude', 'codex', 0.6, TAKE_TEXT['asks']))),
        Shot('claude hears back', 'activity-answer', ('activity-answer', 0.6),
             [(0.6, ('activity-ask', 3.2), ('activity-answer', 0.2)), (3.0, ('activity-answer', 0.2), ('activity-answer', 3.2))],
             ('@claude', TAKE_TEXT['risk']), ('activity-answer', 0.2), 'claude',
             cards=(('codex', 'claude', 0.6, TAKE_TEXT['risk']),)),
        Shot('agents talk', 'collab-thread', ('collab-thread', 0.5),
             [(0.6, ('activity-answer', 3.2), ('collab-thread', -0.2)), (3.2, ('collab-thread', -0.2), ('collab-thread', 3.0))],
             ('@codex to @claude', TAKE_TEXT['risk']), ('collab-thread', -0.2), 'codex'),
    ]),
    ('Two agents in a thread? Pick who gets it.', 'both', [
        Shot('pick', 'pick-hint', 'pick-hint',
             [(1.0, ('pick-hint', -1.4), 'pick-hint'), (2.8, 'pick-hint', ('pick-hint', 3.0))],
             ('', 'Pick who gets this: @claude or @codex'), 'pick-hint', 'codex'),
    ]),
    ('Use an agent\'s skills from the composer', 'claude', [
        Shot('skills', 'skills-list', ('skills-list', 0.5),
             [(1.2, ('skills-open', 0.2), 'skills-list'), (3.0, 'skills-list', ('skills-list', 3.0))],
             ('@claude', '/namedrop'), 'skills-list', 'claude'),
    ]),
    ('Ask for a reminder in a DM', 'claude', [
        Shot('reminder ask', 'reminder-typed', ('reminder-typed', 0.2),
             [(1.5, 'dm-open', 'reminder-typed'), (2.6, 'reminder-typed', ('reminder-typed', 1.75))],
             ('', 'Set a 1-minute reminder here with the text: Tag the release.'), 'reminder-typed', 'claude'),
        Shot('pending', 'reminder-pending', ('reminder-pending', 0.5),
             [(0.6, ('reminder-sent', 0.2), ('reminder-pending', -0.1)), (2.8, ('reminder-pending', -0.1), ('reminder-pending', 2.7))],
             ('Pending reminders', TAKE_TEXT['pending']), 'reminder-pending', 'claude',
             cards=(('you', 'claude', ('reminder-sent', 0.2), 'Set a 1-minute reminder here with the text: Tag the release.'),),
             search={'x': 840, 'y': 40, 'width': 420, 'height': 140}),
        Shot('reminder fires', 'reminder-answer', ('reminder-answer', 0.5),
             [(3.0, ('reminder-answer', -0.2), ('reminder-answer', 2.5))],
             ('@claude', TAKE_TEXT['answer']), 'reminder-answer', 'claude',
             cards=(('claude', 'you', 'reminder-answer', TAKE_TEXT['answer']),), label='1 minute later'),
    ]),
    ('See when an agent needs you', 'claude', [
        Shot('needs you', 'needs-blocked', 'needs-blocked',
             [(0.7, ('needs-sent', 0.3), ('needs-blocked', -0.8)), (2.9, ('needs-blocked', -0.8), ('needs-blocked', 2.1))],
             ('Needs you', '@claude is waiting on an approval or question.'), ('needs-blocked', -0.65), 'claude',
             cards=(('you', 'claude', ('needs-sent', 0.3), 'Use Bash to run: touch release-notes.txt'),),
             search={'x': 8, 'y': 108, 'width': 231, 'height': 80}),
    ]),
    ('See subagents on the member chip', 'claude', [
        Shot('subagent ask', 'sub-typed', ('sub-typed', 0.2),
             [(1.6, ('sub-open', 0.3), 'sub-typed'), (2.6, 'sub-typed', ('sub-typed', 1.75))],
             ('', '@claude Use two subagents to each name one release risk.'), 'sub-typed', 'claude'),
        Shot('subagents', 'sub-count', ('sub-count', 0.3),
             [(0.8, ('sub-sent', 0.2), ('sub-count', -0.85)), (2.6, ('sub-count', -0.85), ('sub-count', 0.71))],
             ('@claude', TAKE_TEXT['subagents']), ('sub-count', -0.85), 'claude',
             cards=(('you', 'claude', ('sub-sent', 0.2), 'Use two subagents to each name one release risk.'),)),
    ]),
]

font_path = args.font or Path(subprocess.run(['fc-match', '-f', '%{file}', 'Liberation Sans:bold'],
                                             check=True, capture_output=True, text=True).stdout)
shutil.copyfile(font_path, args.directory / 'callout-font.ttf')
fonts = {size: ImageFont.truetype(str(font_path), size) for size in (CALLOUT_MAIN, CALLOUT_WHO)}


def font(size):
    if size not in fonts:
        fonts[size] = ImageFont.truetype(str(font_path), size)
    return fonts[size]


def callout_box(ring, who, text):
    """The callout in app CSS px: below the ring when it fits, otherwise above. A long line
    gets a smaller font so the callout stays inside the app."""
    size = CALLOUT_MAIN
    while size > 24 and font(size).getlength(text) + 2 * CALLOUT_PAD > VIEW_W - 16:
        size -= 1
    width = max(font(size).getlength(text), font(CALLOUT_WHO).getlength(who) if who else 0) + 2 * CALLOUT_PAD
    height = 2 * 14 + size * 1.2 + (CALLOUT_WHO * 1.35 if who else 0)
    below, above = ring['y'] + ring['height'] + CALLOUT_GAP, ring['y'] - CALLOUT_GAP - height
    y = below if below + height <= VIEW_H - 8 else above
    x = min(max(ring['x'] + ring['width'] / 2 - width / 2, 8), VIEW_W - 8 - width)
    return {'x': round(x, 1), 'y': round(y, 1), 'width': round(width, 1), 'height': round(height, 1), 'size': size}


def output_time(source, s):
    """The first output time at which the shot shows source time s."""
    for (t0, s0), (t1, s1) in zip(source, source[1:]):
        if s0 <= s <= s1:
            return t0 + (t1 - t0) * ((s - s0) / (s1 - s0) if s1 > s0 else 0)
    return None


def cap_of(size):
    top, bottom = font(size).getbbox('H')[1::2]
    return bottom - top


errors, banned, shots, captions, rows, cards = [], [], [], [], [], []
schedules = {'codex': [], 'claude': []}
active = []
clock = 0.0
for caption, emphasis, scene_shots in SCENES:
    scene_start = clock
    active.append([round(scene_start, 3), emphasis])
    for index, shot in enumerate(scene_shots):
        start, fade = clock, (FADE if shots else 0.0)
        element = events[shot.focus]['focus']
        ring, cap = ink_box(shot.search or element['ring'], at(shot.ink), shot.pad)
        source, t = [], start
        for seconds, source_from, source_to in shot.segments:
            s0, s1 = at(source_from), at(source_to)
            if s1 <= s0:
                errors.append(f'{shot.name}: source runs backwards at {t:.2f}s')
            if not source or abs(source[-1][1] - s0) > 1e-6:
                source.append([t, s0])
            t += seconds
            source.append([t, s1])
        end = t
        hold_seconds, hold_from, hold_to = shot.segments[-1]
        hold = min(end - start - fade, hold_seconds)
        speed = (at(hold_to) - at(hold_from)) / hold_seconds
        who, text = shot.callout
        quotes = [who, text] + [quote for *_, quote in shot.cards]
        for part in quotes + [caption, shot.label]:
            if BANNED.search(part):
                banned.append(f'{shot.name}: "{part}" says "operator"; the person watching is "you"')
        for part in quotes:
            if part and normalize(part) not in SHOWN:
                errors.append(f'{shot.name}: "{part}" is not text the take showed')
        shown = output_time(source, at(shot.shown))
        if shown is None:
            errors.append(f'{shot.name}: the callout text is never on screen in this shot')
            shown = end
        callout_from = max(shown, start + fade)
        if hold < MIN_HOLD - 1e-6:
            errors.append(f'{shot.name}: hold {hold:.2f}s is shorter than {MIN_HOLD}s')
        if end - callout_from < MIN_HOLD - 0.3:
            errors.append(f'{shot.name}: the callout shows for only {end - callout_from:.2f}s')
        # A still screen may play slower than real time; a moving one may not play faster.
        if not 0.6 <= speed <= 1.15:
            errors.append(f'{shot.name}: the hold plays at {speed:.2f}x')
        in_app = cap * APP_SCALE * README_WIDTH / W
        box = callout_box(ring, who, text)
        in_callout = cap_of(box['size']) * APP_SCALE * README_WIDTH / W
        if max(in_app, in_callout) < MIN_CAP_README:
            errors.append(f'{shot.name}: focal text is {in_app:.1f}px in the app and {in_callout:.1f}px in the callout at {README_WIDTH}px')
        for sender, recipient, when, quote in shot.cards:
            t0 = start + when if isinstance(when, float) else output_time(source, at(when))
            if t0 is None:
                errors.append(f'{shot.name}: card "{quote}" is not inside the shot')
                continue
            cards.append({'from': sender, 'to': recipient, 't0': round(t0, 3), 'text': quote})
        for alias in schedules:
            for at_output, at_source in ((start, source[0][1]), (end - hold, at(hold_from))):
                key = pane_key(alias, at_source)
                if not schedules[alias] or schedules[alias][-1][1] != key:
                    schedules[alias].append([round(at_output, 3), key])
        # The spotlight eases in once; later shots arrive lit, so the dim never pulses between shots.
        spot = [[0.4, 0], [0.4 + SPOT_IN, 1], [round(end, 3), 1]] if not shots else [[round(start, 3), 1], [round(end, 3), 1]]
        shot.plan = {'hold': hold, 'speed': speed, 'inApp': in_app, 'inCallout': in_callout,
                     'callout': (who + ': ' if who else '') + text, 'calloutSeconds': end - callout_from}
        shots.append({'name': shot.name, 'start': round(start, 3), 'end': round(end, 3), 'fade': fade,
                      'source': [[round(a, 3), round(b, 3)] for a, b in source],
                      'spot': spot,
                      'ring': {k: round(v, 2) for k, v in ring.items()},
                      'callout': {'who': who, 'text': text, 'from': round(callout_from, 3),
                                  'box': box},
                      'hold': [round(end - hold, 3), round(end, 3)], 'capCss': cap, 'agent': shot.agent,
                      **({'label': shot.label} if shot.label else {})})
        rows.append((caption if index == 0 else '', shot, start, end, fade))
        clock = end
    captions.append({'start': round(scene_start, 3), 'end': round(clock, 3), 'text': caption})

duration = round(clock + END_SECONDS, 3)
if not DURATION[0] <= duration <= DURATION[1]:
    errors.append(f'{duration}s is outside {DURATION[0]}-{DURATION[1]}s')
for caption in captions:
    if caption['end'] - caption['start'] < 3:
        errors.append(f'caption "{caption["text"]}" lasts {caption["end"] - caption["start"]:.2f}s')
banned += [f'agent screen {key} says "operator": {plain(row).strip()}' for key, frame in FRAMES.items()
           for row in frame['rows'] if BANNED.search(plain(row))]
if args.rehearsal:
    print('\n'.join('warning: ' + line for line in banned))
else:
    errors += banned
if errors:
    parser.error('\n'.join(errors))

(args.directory / 'panes.js').write_text('window.FRAMES = ' + json.dumps(
    {key: {'rows': frame['rows'], 'cols': frame['cols']} for key, frame in FRAMES.items()}) + ';\n')
timeline = {'duration': duration, 'end': round(clock, 3), 'viewport': recording['viewport'],
            'app': {**APP, 'left': APP_LEFT, 'top': APP_TOP, 'bar': BAR},
            'agents': {alias: {'top': AGENT_TOPS[alias], 'column': COLUMN, 'rows': AGENT_ROWS, 'schedule': schedules[alias]}
                       for alias in schedules},
            'active': active, 'cards': cards,
            'callout': {'font': 'callout-font.ttf', 'main': CALLOUT_MAIN, 'who': CALLOUT_WHO, 'pad': CALLOUT_PAD},
            'shots': shots, 'captions': captions}
(args.directory / 'timeline.json').write_text(json.dumps(timeline, indent=1) + '\n')
print(f'{len(recording["frames"])} real source frames, {len(FRAMES)} real agent screens. {len(shots)} shots, '
      f'{len(cards)} cards, no camera moves, {len(captions)} captions, {duration:g} seconds planned.')
for _, shot, *_ in rows:
    print(f'  {shot.name:<15} hold {shot.plan["hold"]:.1f}s at {shot.plan["speed"]:.2f}x  callout {shot.plan["calloutSeconds"]:.1f}s  '
          f'cap at {README_WIDTH}: app {shot.plan["inApp"]:.1f}px, callout {shot.plan["inCallout"]:.1f}px')

if args.shotlist:
    lines = ['# Channels highlight shot list', '',
             'Generated by `docs/demo/channels/motion/plan.py --shotlist`. The whole Channels window is on screen in '
             'every shot and the camera does not move. Times are output seconds. Source is seconds into the raw take. '
             'Speed is source seconds per output second. Cap heights are at 900 px README width: the smallest focal '
             'text in the app, measured in the source frame, and the callout text.', '',
             '| Caption | Shot | Start | End | Entry | Hold | Source | Speed | App cap | Callout cap | Callout (real text) |',
             '|---|---|---:|---:|---|---:|---|---|---:|---:|---|']
    for caption, shot, start, end, fade in rows:
        speeds = ', '.join(f'{(at(b) - at(a)) / s:.1f}x' for s, a, b in shot.segments)
        entry = f'{fade:g}s dissolve' if fade else 'first shot'
        lines.append(f'| {caption} | {shot.name} | {start:.1f} | {end:.1f} | {entry} | {shot.plan["hold"]:.1f}s '
                     f'| {at(shot.segments[0][1]):.1f} to {at(shot.segments[-1][2]):.1f} | {speeds} '
                     f'| {shot.plan["inApp"]:.1f} px | {shot.plan["inCallout"]:.1f} px | {shot.plan["callout"]} |')
    lines += [f'| | end card | {clock:.1f} | {duration:.1f} | | | | | | | |', '',
              '| Card | Output time | Real message |', '|---|---:|---|',
              *[f'| {c["from"]} to {c["to"]} | {c["t0"]:.1f} | {c["text"]} |' for c in cards]]
    args.shotlist.parent.mkdir(parents=True, exist_ok=True)
    args.shotlist.write_text('\n'.join(lines) + '\n')
