# Continuous Channels recording

This is the round-two camera pipeline. The existing published assets remain the
first cut until the exp.17 capture is approved. The rehearsal records the real
whole app and moves a camera over that recording. It never redraws interface text.

`rehearse.mjs` records an isolated **Release rehearsal** channel at 1440 by 900
with `deviceScaleFactor: 2`. It captures timestamped 2880 by 1800 JPEG frames while
Playwright types and sends real messages. `recording.json` keeps each frame's
source time and marks typing, posting, reply arrival, and thread opening. Its wall-clock
start aligns these frames with the epoch timestamps from pane captures.
`capture.sh` separately records real pane output using the same isolated profile.

`scene.mjs` maps output time to source time, then draws the corresponding whole-app
frame. Camera keyframes use source CSS coordinates, a scale, and the vertical
position of the point of interest in the output frame. Smoothstep eases
between positions. During a close zoom, a small live overview shows the entire app
and outlines the current field of view. Waiting can be compressed independently
of typing and camera motion. The shared `render.mjs` produces deterministic output
frames and ffmpeg encodes them.

## Rehearse the pipeline

Use only an isolated testbed profile and its loopback URL. Start with an empty
**Release rehearsal** channel and a warmed, real Claude session. Keep the URL out
of source control. From the repository root:

```sh
CHROME=/path/to/chromium node docs/demo/channels/motion/rehearse.mjs "$CHANNELS_URL" /path/to/rehearsal/recording bcae742 --rehearsal
python3 docs/demo/channels/motion/plan.py /path/to/rehearsal
cp docs/demo/channels/motion/scene.html docs/demo/channels/motion/scene.mjs /path/to/rehearsal/
python3 -m http.server 8770 --bind 127.0.0.1 --directory /path/to/rehearsal
```

In another shell, render the 28-second proof:

```sh
SCENE_URL=http://127.0.0.1:8770/scene.html CHROME=/path/to/chromium RENDER_SCALE=1.3333333333333333 node docs/demo/render.mjs /path/to/rehearsal/pipeline.mp4 24
```

The rehearsal has an explicit old-build caption. Its known old-build statuses are
not final footage. The shipped source revision must be recorded accurately in the
manifest. Keep captures and builds outside `/tmp`.

## Final capture gate

Wait for the coordinator's exp.17 SHA. Install that exact commit into a fresh
`test-` profile, verify its source revision and embedded bundle, and warm both real
agents. Use a short directory inside the profile HOME so terminal paths use `~`.
Confirm Live Peek survives Type to agent, all status labels agree, and the pending
reminder no longer says "0 repeats left" before filming.

Record the channel ask, plain thread follow-up, DM reminder, Needs you, and Live
Peek as one continuous workspace. Extend the source-event plan to 45–60 seconds.
Hold completed instructions long enough to read, ease out on send, pan toward the
working pane or reply, and return to the wide view. Label compressed reminder
waiting as **2 minutes later**. Cancel every captured permission prompt. Keep the
final MP4 below about 15 MB and the 1400 px WebP below about 8 MB. The poster may be
a shorter excerpt.
