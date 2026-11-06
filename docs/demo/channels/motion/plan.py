"""Plan the Channels highlight: shots, camera keyframes, source time and captions."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('directory', type=Path, help='take directory with recording/recording.json')
parser.add_argument('--shotlist', type=Path, help='also write a Markdown shot list here')
args = parser.parse_args()
recording = json.loads((args.directory / 'recording/recording.json').read_text())
events = {event['name']: event['t'] for event in recording['events']}

PICTURE_W, PICTURE_H = 1920, 925
MAX_DURATION = 59.5
MAX_ZOOM = 1.4
MOVE_SECONDS = (1.0, 1.6)
MIN_HOLD = 2.0
MIN_READ = 2.4
MIN_CAPTION = 2.5

# Close-ups crop the full app width (1440 CSS px) and drop the agent panes, so the
# viewer always sees the whole app across and only loses its top or bottom.
APP_ZOOM = PICTURE_W / 1440
FRAMINGS = {
    'wide': [960, 450, 0.96],
    'app-bottom': [720, 900 - PICTURE_H / APP_ZOOM / 2, APP_ZOOM],
    'app-top': [720, PICTURE_H / APP_ZOOM / 2, APP_ZOOM],
}


def at(point):
    if isinstance(point, str):
        return events[point]
    name, offset = point
    return events[name] + offset


def hold(seconds, source_from, source_to, caption=None):
    return {'seconds': seconds, 'source': (source_from, source_to), 'move': None, 'caption': caption}


def move(seconds, source_from, source_to, framing, caption=None):
    return {'seconds': seconds, 'source': (source_from, source_to), 'move': framing, 'caption': caption}


# Each beat opens on the wide view, makes one eased push-in, then holds. Later beats
# start with a short dissolve. The DM beat dissolves between its close-ups instead of
# panning, and the two-minute wait is a dissolve in time.
SHOTS = [
    ('Channel ask', 'wide', 0, [
        hold(2.5, ('wide', 0.1), 'ask-type', 'Your agents. One workspace.'),
        move(1.3, 'ask-type', ('ask-type', 2.4), 'app-bottom', 'Send an instruction to one agent.'),
        hold(1.5, ('ask-type', 2.4), 'ask-instruction'),
        hold(2.5, 'ask-instruction', ('ask-posted', -0.05)),
    ]),
    ('Codex works', 'wide', 0.5, [
        hold(2.6, 'ask-posted', 'ask-reply', 'Codex gets to work in its own session.'),
        move(1.3, 'ask-reply', ('ask-reply', 0.6), 'app-bottom', 'The reply lands in the channel.'),
        hold(2.6, ('ask-reply', 0.6), ('thread-open', -0.05)),
    ]),
    ('Thread follow-up', 'wide', 0.5, [
        hold(2.0, 'thread-open', ('followup-type', 1.1), 'Follow up in the thread. No @ needed.'),
        move(1.3, ('followup-type', 1.1), 'followup-instruction', 'app-bottom'),
        hold(2.4, 'followup-instruction', ('followup-posted', -0.05)),
        hold(1.8, 'followup-posted', 'followup-reply', 'The thread keeps its agent.'),
        hold(2.5, 'followup-reply', ('followup-reply', 1.2)),
    ]),
    ('DM reminder', 'wide', 0.5, [
        hold(2.0, ('reminder-type', 2.6), ('reminder-type', 5), 'Schedule a reminder in a DM.'),
        move(1.3, ('reminder-type', 5), 'reminder-instruction', 'app-bottom'),
        hold(2.4, 'reminder-instruction', ('reminder-posted', -0.05)),
    ]),
    ('Reminder pending', 'app-top', 0.5, [
        hold(1.0, 'reminder-posted', 'reminder-pending', 'Pending. You can still cancel.'),
        hold(2.6, 'reminder-pending', ('reminder-pending', 2.5)),
    ]),
    ('Reminder fires', 'app-bottom', 0.8, [
        hold(1.2, 'reminder-fired', 'reminder-reply', 'Two minutes later, Claude follows through.'),
        hold(2.8, 'reminder-reply', ('reminder-reply', 1.9)),
    ]),
    ('Needs you', 'wide', 0.5, [
        hold(1.2, 'needs-request', 'needs-blocked', 'Know when an agent needs you.'),
        hold(1.6, 'needs-blocked', ('needs-blocked', 1.7)),
        move(1.3, ('needs-blocked', 1.7), ('needs-blocked', 3.0), 'app-top', 'Find it in Needs you.'),
        hold(2.8, ('needs-blocked', 3.0), ('needs-blocked', 5.7)),
    ]),
    ('Live Peek', 'wide', 0.5, [
        hold(2.0, ('peek-open', -0.55), ('peek-open', 0.85), 'Look into a live session with Peek.'),
        move(1.3, ('peek-open', 0.85), ('peek-type', 3.8), 'app-bottom'),
        hold(1.2, ('peek-type', 3.8), 'peek-instruction', 'Type to the agent from Peek.'),
        hold(2.4, 'peek-instruction', ('peek-posted', -0.1)),
        move(1.5, ('peek-posted', -0.1), ('peek-reply', -0.1), 'app-top', 'The reply arrives live.'),
        hold(3.0, ('peek-reply', -0.1), 'end'),
    ]),
]
END_SECONDS, END_FADE = 2.8, 0.6

errors = []
shots, captions, rows = [], [], []
time = 0.0
for beat, framing, fade, phases in SHOTS:
    view = FRAMINGS[framing]
    start = time
    camera, source = [[time, view]], [[time, at(phases[0]['source'][0])]]
    moves, still_from = 0, start
    for phase in phases:
        source_from, source_to = at(phase['source'][0]), at(phase['source'][1])
        if source_to < source_from or source_from < source[-1][1] - 1e-6:
            errors.append(f'{beat}: source time runs backwards at {time:.2f}s')
        if abs(source_from - source[-1][1]) > 1e-6:
            source.append([time, source_from])
        if phase['caption']:
            if captions:
                captions[-1]['end'] = round(time, 3)
            captions.append({'start': round(time, 3), 'end': None, 'text': phase['caption']})
        if phase['move']:
            moves += 1
            low, high = MOVE_SECONDS
            if not low <= phase['seconds'] <= high:
                errors.append(f'{beat}: a {phase["seconds"]}s move is outside {low}-{high}s')
            if time - still_from < MIN_HOLD - 1e-6:
                errors.append(f'{beat}: the camera holds only {time - still_from:.2f}s before moving at {time:.2f}s')
            camera.append([time, view])
            view = FRAMINGS[phase['move']]
            camera.append([time + phase['seconds'], view])
            still_from = time + phase['seconds']
        time += phase['seconds']
        source.append([time, source_to])
    camera.append([time, view])
    if time - still_from < MIN_READ - 1e-6:
        errors.append(f'{beat}: final hold {time - still_from:.2f}s is shorter than {MIN_READ}s')
    if moves > 2:
        errors.append(f'{beat}: {moves} camera moves')
    shots.append({'beat': beat, 'start': round(start, 3), 'end': round(time, 3), 'fade': fade,
                  'source': [[round(t, 3), round(s, 3)] for t, s in source],
                  'camera': [[round(t, 3), [round(v, 3) for v in view]] for t, view in camera]})
    rows.append((beat, start, framing, fade, phases))
captions[-1]['end'] = round(time, 3)
last = shots[-1]
shots.append({'beat': 'End card', 'start': round(time, 3), 'end': round(time + END_SECONDS, 3), 'fade': END_FADE,
              'endCard': True, 'source': last['source'][-1:] * 2, 'camera': last['camera'][-1:] * 2})
duration = round(time + END_SECONDS, 3)

for name, view in FRAMINGS.items():
    if view[2] > MAX_ZOOM:
        errors.append(f'{name}: zoom {view[2]:.2f} exceeds {MAX_ZOOM}')
for caption in captions:
    if caption['end'] - caption['start'] < MIN_CAPTION:
        errors.append(f'caption "{caption["text"]}" lasts {caption["end"] - caption["start"]:.2f}s')
if duration > MAX_DURATION:
    errors.append(f'{duration}s exceeds {MAX_DURATION}s')
if errors:
    parser.error('\n'.join(errors))

(args.directory / 'timeline.json').write_text(json.dumps({'duration': duration, 'shots': shots, 'captions': captions}, indent=1) + '\n')
print(f'{len(recording["frames"])} real source frames. {len(shots)} shots, {len(captions)} captions, {duration:g} seconds planned.')

if args.shotlist:
    lines = ['# Channels highlight shot list', '',
             'Generated by `docs/demo/channels/motion/plan.py --shotlist`. Times are output seconds. '
             'Source times are seconds into the raw take. Speed is source seconds per output second; '
             'below 1x the still frame is held a little longer than it was filmed.', '',
             'Framings: `wide` shows the whole app and both agent panes at zoom 0.96. `app-bottom` and `app-top` '
             f'crop the full app width at zoom {APP_ZOOM:.3f} and keep its lower or upper part.', '',
             '| Beat | Start | End | Entry | Framing | Move | Duration | Source | Speed | Caption |',
             '|---|---:|---:|---|---|---|---:|---|---:|---|']
    for beat, clock, current, fade, phases in rows:
        for i, phase in enumerate(phases):
            s0, s1 = at(phase['source'][0]), at(phase['source'][1])
            target = phase['move']
            kind = 'hold' if not target else ('eased push-in' if current == 'wide' else 'eased tilt')
            entry = ('cut' if not fade else f'{fade:g}s dissolve') if i == 0 else ''
            lines.append(f'| {beat if i == 0 else ""} | {clock:.1f} | {clock + phase["seconds"]:.1f} | {entry} '
                         f'| {current + " to " + target if target else current} | {kind} | {phase["seconds"]:.1f} '
                         f'| {s0:.1f} to {s1:.1f} | {(s1 - s0) / phase["seconds"]:.1f}x | {phase["caption"] or ""} |')
            clock += phase['seconds']
            current = target or current
    lines.append(f'| End card | {time:.1f} | {duration:.1f} | {END_FADE:g}s fade | over the last frame | hold | {END_SECONDS:.1f} | | | |')
    args.shotlist.parent.mkdir(parents=True, exist_ok=True)
    args.shotlist.write_text('\n'.join(lines) + '\n')
