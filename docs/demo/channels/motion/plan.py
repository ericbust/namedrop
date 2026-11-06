"""Plan the Channels highlight from a take: shots, focal rings, camera, spotlight and captions.

Every focal ring is the tight box of real ink inside an element the recorder measured, found
in the source frame itself. The planner fails when a move, hold, zoom, caption, total length
or focal text size breaks the storyboard rules."""
import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path

import numpy as np
from PIL import Image

from ink import ink_mask, line_caps

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('directory', type=Path, help='take directory with recording/recording.json')
parser.add_argument('--shotlist', type=Path, help='also write a Markdown shot list here')
args = parser.parse_args()
recording = json.loads((args.directory / 'recording/recording.json').read_text())
events = {event['name']: event for event in recording['events']}
VIEW_W, VIEW_H = recording['viewport']['width'], recording['viewport']['height']
DPR = recording['deviceScaleFactor']

# Output is 1920×1080 logical px, rendered at 4/3 to 2560×1440. A caption band sits on top.
W, H, BAND = 1920, 1080, 110
SCALE = (H - BAND) / VIEW_H
README_WIDTH = 900
MAX_ZOOM, PUSH, MIN_HOLD, FADE = 2.0, 1.2, 2.5, 0.45
MIN_CAP_README = 9.5
DURATION = (30, 40)
END_SECONDS, END_FADE = 2.8, 0.6


def at(point):
    name, offset = point if isinstance(point, tuple) else (point, 0)
    return events[name]['t'] + offset


def frame_at(t):
    return min(recording['frames'], key=lambda frame: abs(frame['t'] - t))


def ink_box(rect, t, pad):
    """The padded bounding box, in CSS px, of the ink inside rect, and the smallest cap height
    of its text lines in CSS px, both read from the source frame."""
    image = Image.open(args.directory / 'recording' / frame_at(t)['file']).convert('L')
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


@dataclass
class Shot:
    name: str
    focus: str                # event whose measured element holds the focal text
    ink: object               # source time of the frame the ring is found in
    segments: list            # (output seconds, source from, source to); the last one is the hold
    push: float = 0.0         # seconds on the wide view before an eased push-in; 0 is a cut
    search: dict = None       # search rectangle in CSS px instead of the event's element
    max_zoom: float = MAX_ZOOM
    label: str = ''
    pad: float = 12
    plan: dict = field(default_factory=dict)


SCENES = [
    ('Ask an agent in a channel', [
        Shot('ask', 'ask-typed', ('ask-typed', 0.2), [(4.5, ('ask-wide', 1.4), ('ask-typed', 1.75))], push=0.6),
        Shot('reply', 'ask-reply', ('ask-reply', 0.5), [(0.8, ('ask-sent', 0.2), ('ask-reply', -0.2)),
                                                        (2.6, ('ask-reply', -0.2), ('ask-reply', 2.4))]),
    ]),
    ('Follow up in the thread. No @ needed.', [
        Shot('follow-up', 'followup-typed', ('followup-typed', 0.2), [(3.0, ('followup-open', -0.1), ('followup-sent', -0.05))]),
        Shot('thread reply', 'followup-reply', ('followup-reply', 0.5), [(0.7, ('followup-sent', 0.2), ('followup-reply', -0.2)),
                                                                        (2.6, ('followup-reply', -0.2), ('followup-reply', 2.4))]),
    ]),
    ('Ask for a reminder in a DM', [
        Shot('reminder ask', 'reminder-typed', ('reminder-typed', 0.2), [(4.1, 'dm-open', ('reminder-typed', 1.75))], push=0.4),
        Shot('pending', 'reminder-pending', ('reminder-pending', 0.5), [(0.6, ('reminder-sent', 0.2), ('reminder-pending', -0.1)),
                                                                       (2.6, ('reminder-pending', -0.1), ('reminder-pending', 2.5))],
             search={'x': 840, 'y': 40, 'width': 400, 'height': 140}),
        Shot('reminder fires', 'reminder-answer', ('reminder-answer', 0.5), [(3.0, ('reminder-answer', -0.2), ('reminder-answer', 2.5))],
             label='1 minute later'),
    ]),
    ('See when an agent needs you', [
        Shot('needs you', 'needs-blocked', 'needs-blocked', search={'x': 8, 'y': 108, 'width': 231, 'height': 80}, segments=[(0.7, ('needs-sent', 0.3), ('needs-blocked', -0.7)),
                                                             (2.7, ('needs-blocked', -0.7), ('needs-blocked', 2.0))]),
    ]),
    ('Watch an agent live and type to it', [
        Shot('peek input', 'peek-typed', ('peek-typed', 0.2), [(4.1, 'peek-open', ('peek-typed', 1.8))], push=0.4,
             search={'x': 267, 'y': 585, 'width': 453, 'height': 70}),
        Shot('peek reply', 'peek-reply', ('peek-reply', 0.5), [(0.9, ('peek-sent', 0.2), ('peek-reply', -0.3)),
                                                              (2.6, ('peek-reply', -0.3), ('peek-reply', 2.3))],
             search={'x': 266, 'y': 104, 'width': 998, 'height': 160}, max_zoom=1.5),
    ]),
]


def framing(ring, max_zoom):
    """Camera [cx, cy, zoom] in CSS px that fits the ring, clamped to stay inside the app."""
    fit = min(W / (SCALE * (ring['width'] + 80)), (H - BAND) / (SCALE * (ring['height'] + 60)))
    zoom = max(1.0, min(max_zoom, fit))
    half_w, half_h = W / (2 * SCALE * zoom), (H - BAND) / (2 * SCALE * zoom)
    cx, cy = ring['x'] + ring['width'] / 2, ring['y'] + ring['height'] / 2
    cx = VIEW_W / 2 if half_w >= VIEW_W / 2 else min(max(cx, half_w), VIEW_W - half_w)
    cy = VIEW_H / 2 if half_h >= VIEW_H / 2 else min(max(cy, half_h), VIEW_H - half_h)
    return [cx, cy, zoom], fit


WIDE = [VIEW_W / 2, VIEW_H / 2, 1.0]
errors, shots, captions, rows = [], [], [], []
clock = 0.0
for caption, scene_shots in SCENES:
    scene_start = clock
    for index, shot in enumerate(scene_shots):
        start, fade = clock, (FADE if shots else 0.0)
        element = events[shot.focus]['focus']
        ring, cap = ink_box(shot.search or element['ring'], at(shot.ink), shot.pad)
        view, fit = framing(ring, shot.max_zoom)
        if fit < 1.0:
            errors.append(f'{shot.name}: the ring is too large to frame at zoom 1')
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
        if shot.push:
            arrive = start + shot.push + PUSH
            camera = [[start, WIDE], [start + shot.push, WIDE], [arrive, view], [end, view]]
            spot = [[start + shot.push + PUSH * 0.4, 0], [arrive, 1], [end, 1]]
            hold = end - arrive
        else:
            arrive = start + fade
            camera = [[start, view], [end, view]]
            spot = [[start, 1], [end, 1]]
            hold = min(end - arrive, hold_seconds)
        speed = (at(hold_to) - at(hold_from)) / hold_seconds
        if hold < MIN_HOLD - 1e-6:
            errors.append(f'{shot.name}: focal hold {hold:.2f}s is shorter than {MIN_HOLD}s')
        if not 0.85 <= speed <= 1.15:
            errors.append(f'{shot.name}: the hold plays at {speed:.2f}x')
        readme_cap = cap * SCALE * view[2] * README_WIDTH / W
        if readme_cap < MIN_CAP_README:
            errors.append(f'{shot.name}: focal cap height {readme_cap:.1f}px at {README_WIDTH}px is under {MIN_CAP_README}')
        shot.plan = {'zoom': view[2], 'hold': hold, 'capCss': cap, 'domCapCss': element['read'].get('capPx'),
                     'readmeCap': readme_cap, 'text': element['read']['text']}
        shots.append({'name': shot.name, 'start': round(start, 3), 'end': round(end, 3), 'fade': fade,
                      'source': [[round(a, 3), round(b, 3)] for a, b in source],
                      'camera': [[round(a, 3), [round(c[0], 2), round(c[1], 2), round(c[2] * SCALE, 4)]] for a, c in camera],
                      'spot': [[round(a, 3), b] for a, b in spot],
                      'ring': {k: round(v, 2) for k, v in ring.items()},
                      'hold': [round(arrive, 3), round(end, 3)],
                      'capCss': cap, 'zoom': round(view[2], 4),
                      **({'label': shot.label} if shot.label else {})})
        rows.append((caption if index == 0 else '', shot, start, end, fade))
        clock = end
    captions.append({'start': round(scene_start, 3), 'end': round(clock, 3), 'text': caption})

last = shots[-1]
shots.append({'name': 'end card', 'start': round(clock, 3), 'end': round(clock + END_SECONDS, 3), 'fade': END_FADE,
              'endCard': True, 'source': last['source'][-1:] * 2, 'camera': last['camera'][-1:] * 2, 'spot': [[0, 0]]})
duration = round(clock + END_SECONDS, 3)
if not DURATION[0] <= duration <= DURATION[1]:
    errors.append(f'{duration}s is outside {DURATION[0]}-{DURATION[1]}s')
for caption in captions:
    if caption['end'] - caption['start'] < 3:
        errors.append(f'caption "{caption["text"]}" lasts {caption["end"] - caption["start"]:.2f}s')
if errors:
    parser.error('\n'.join(errors))

timeline = {'duration': duration, 'band': BAND, 'scale': SCALE, 'viewport': recording['viewport'],
            'shots': shots, 'captions': captions}
(args.directory / 'timeline.json').write_text(json.dumps(timeline, indent=1) + '\n')
pushes = sum(1 for _, shot, *_ in rows if shot.push)
print(f'{len(recording["frames"])} real source frames. {len(shots)} shots, {pushes} push-ins, '
      f'{len(captions)} captions, {duration:g} seconds planned.')
for _, shot, *_ in rows:
    print(f'  {shot.name:<15} zoom {shot.plan["zoom"]:.2f}  hold {shot.plan["hold"]:.1f}s  '
          f'cap {shot.plan["capCss"]:.1f} CSS px (DOM {shot.plan["domCapCss"] or 0:.1f}) -> {shot.plan["readmeCap"]:.1f} px at {README_WIDTH}')

if args.shotlist:
    lines = ['# Channels highlight shot list', '',
             'Generated by `docs/demo/channels/motion/plan.py --shotlist`. Times are output seconds. Source is seconds '
             'into the raw take. Speed is source seconds per output second. Zoom 1 shows the whole app. The cap '
             'height is that of the smallest focal text line, measured in the source frame, as it appears in a 900 px wide README.', '',
             '| Caption | Shot | Start | End | Entry | Zoom | Hold | Source | Speed | Cap at 900 px | Focal text |',
             '|---|---|---:|---:|---|---:|---:|---|---|---:|---|']
    for caption, shot, start, end, fade in rows:
        speeds = ', '.join(f'{(at(b) - at(a)) / s:.1f}x' for s, a, b in shot.segments)
        entry = (f'{shot.push:g}s wide, {PUSH:g}s push-in' if shot.push else 'cut') + (f', {fade:g}s dissolve' if fade else '')
        lines.append(f'| {caption} | {shot.name} | {start:.1f} | {end:.1f} | {entry} | {shot.plan["zoom"]:.2f} '
                     f'| {shot.plan["hold"]:.1f}s | {at(shot.segments[0][1]):.1f} to {at(shot.segments[-1][2]):.1f} '
                     f'| {speeds} | {shot.plan["readmeCap"]:.1f} px | {shot.plan["text"][:70]} |')
    lines.append(f'| | end card | {clock:.1f} | {duration:.1f} | {END_FADE:g}s fade | | | | | | |')
    args.shotlist.parent.mkdir(parents=True, exist_ok=True)
    args.shotlist.write_text('\n'.join(lines) + '\n')
