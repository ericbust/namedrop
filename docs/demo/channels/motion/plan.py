import argparse
import json
from pathlib import Path


parser = argparse.ArgumentParser(description='Plan a continuous Channels highlight from recorded events.')
parser.add_argument('directory', type=Path)
parser.add_argument('--rehearsal', action='store_true', help='Map old rehearsal event names for camera checks only.')
args = parser.parse_args()
recording = json.loads((args.directory / 'recording/recording.json').read_text())
events = {event['name']: event for event in recording['events']}
for transient, preceding in {
    'ask-working': 'ask-posted', 'reminder-confirmed': 'reminder-pending',
    'needs-blocked': 'needs-request', 'peek-working': 'peek-posted',
}.items():
    if transient not in events and preceding in events:
        events[transient] = events[preceding]

if args.rehearsal:
    aliases = {
        'ask-type': 'type', 'ask-instruction': 'instruction', 'ask-posted': 'posted',
        'ask-working': 'posted', 'ask-reply': 'reply', 'thread-open': 'thread',
        'followup-instruction': 'followup', 'followup-reply': 'end',
    }
    for name, old in aliases.items():
        if name not in events and old in events:
            events[name] = events[old]

wide = [960, 450, 0.92, 580]
cameras = {
    'wide': wide,
    'composer': [970, 710, 1.35, 690],
    'reply': [960, 400, 1.3, 550],
    'thread': [1150, 690, 1.5, 680],
    'pending': [950, 580, 1.45, 600],
    'needs': [700, 400, 1.4, 570],
    'peek': [1110, 460, 1.35, 580],
    'codex': [1695, 220, 1.65, 580],
    'claude': [1695, 680, 1.65, 580],
}


def camera(name, event):
    rect = events.get(event, {}).get('rect')
    if name in ('wide', 'codex', 'claude') or not rect:
        return cameras[name]
    x, y, width, height = (rect[key] for key in ('x', 'y', 'width', 'height'))
    zoom = max(1.05, min(1.65, 1550 / (width + 120), 700 / (height + 120)))
    return [x + width / 2, y + height / 2, zoom, 590]


# Each row owns output duration, source interval, camera destination and caption.
shots = [
    (2, 'wide', 'ask-type', 'wide', 'Your agents. One workspace.'),
    (2.3, 'ask-type', 'ask-instruction', 'composer', 'Send an instruction.'),
    (2, 'ask-instruction', 'ask-posted', 'composer', 'Send an instruction.'),
    (1.4, 'ask-posted', 'ask-working', 'codex', 'Codex gets to work.'),
    (2.3, 'ask-working', 'ask-reply', 'reply', 'The reply lands here.'),
    (2, 'ask-reply', 'thread-open', 'reply', 'The reply lands here.'),
    (1, 'thread-open', 'followup-type', 'thread', 'Keep the conversation in a thread.'),
    (2.2, 'followup-type', 'followup-instruction', 'thread', 'Follow up without @.'),
    (1.8, 'followup-instruction', 'followup-posted', 'thread', 'Follow up without @.'),
    (2, 'followup-posted', 'followup-reply', 'thread', 'The thread keeps its agent.'),
    (1.5, 'followup-reply', 'dm-open', 'wide', 'A direct line to Claude.'),
    (1.5, 'dm-open', 'reminder-type', 'composer', 'Schedule a reminder.'),
    (2, 'reminder-type', 'reminder-instruction', 'composer', 'Schedule a reminder.'),
    (2, 'reminder-instruction', 'reminder-posted', 'composer', 'Schedule a reminder.'),
    (2, 'reminder-posted', 'reminder-pending', 'pending', 'Pending. You can still cancel.'),
    (2, 'reminder-pending', 'reminder-confirmed', 'pending', 'Your reminder has a home.'),
    (1.5, 'reminder-confirmed', 'reminder-fired', 'wide', '2 minutes later'),
    (1, 'reminder-fired', 'reminder-fired', 'claude', 'Claude gets to work.'),
    (1.5, 'reminder-fired', 'reminder-reply', 'reply', 'Claude follows through.'),
    (1.5, 'reminder-reply', 'needs-request', 'wide', 'Know when an agent needs you.'),
    (2, 'needs-request', 'needs-blocked', 'claude', 'Claude needs a decision.'),
    (1.5, 'needs-blocked', 'needs-open', 'needs', 'Find it in Needs you.'),
    (2, 'needs-open', 'needs-cancelled', 'needs', 'Review it. Cancel when needed.'),
    (1.5, 'needs-cancelled', 'peek-open', 'wide', 'Look into the live session.'),
    (1.5, 'peek-open', 'peek-type', 'peek', 'Type to the agent from Peek.'),
    (2, 'peek-type', 'peek-instruction', 'peek', 'Type to the agent from Peek.'),
    (2, 'peek-instruction', 'peek-posted', 'peek', 'Type to the agent from Peek.'),
    (2, 'peek-posted', 'peek-working', 'peek', 'Watch the work happen.'),
    (2, 'peek-working', 'peek-reply', 'peek', 'Watch the reply arrive.'),
    (1, 'peek-reply', 'end', 'wide', 'Your agents. One workspace.'),
]

required = {event for shot in shots for event in shot[1:3]}
missing = required - events.keys()
if missing and not args.rehearsal:
    parser.error('Recording is missing events: ' + ', '.join(sorted(missing)))
if args.rehearsal:
    for name in missing:
        events[name] = events['end']

timeline = []
previous_camera = wide
time = 0
for duration, source_from, source_to, destination, caption in shots:
    target = camera(destination, source_to)
    if source_from.endswith('instruction') and timeline and timeline[-1]['focus'] == destination:
        target = previous_camera
    timeline.append({
        'start': round(time, 3), 'end': round(time + duration, 3),
        'source': [events[source_from]['t'], events[source_to]['t']],
        'from': previous_camera, 'to': target, 'caption': caption, 'focus': destination,
    })
    time += duration
    previous_camera = target
timeline.append({
    'start': round(time, 3), 'end': round(time + 3, 3),
    'source': [events['end']['t'], events['end']['t']],
    'from': wide, 'to': wide, 'caption': '', 'endCard': True,
})
(args.directory / 'timeline.json').write_text(json.dumps(timeline, indent=2) + '\n')
print(f'{len(recording["frames"])} real source frames. {time + 3:g} seconds planned.')
