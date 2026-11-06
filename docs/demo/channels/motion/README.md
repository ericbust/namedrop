# Record and render the Channels highlight

The README movie is one continuous real take of the Channels page, cut into five short scenes. Each scene has one caption and one focal element at a time. The app is recorded at a 1280×720 CSS viewport with `deviceScaleFactor: 2`, so each source frame is 2560×1440 and the UI text is large in the movie.

Only crops, scaling, cuts, dissolves and overlays are added. The overlays are the caption band, a spotlight that dims app pixels outside the focal ring, the ring itself, the **1 minute later** label and the end card. No app text is drawn or changed.

`capture.json` records the published take: product and source revisions, event times, and a hash of the source frame behind each focal hold.

| File | Purpose |
|---|---|
| `stage.py` | Builds a clean take in a `test-` profile: a new empty workspace named **Release planning**, both agents started in `~/release-planning`, warmed up and idle, and Claude in manual permission mode |
| `record.mjs` | Opens Channels in Chromium, records timestamped 2x frames, and runs queued step files. `mark()` stores each event with the measured focal element |
| `queue.mjs` | Queues one step file for the recorder |
| `steps/*.js` | The six capture steps, typed and sent through the real UI |
| `profile.py` | The only bridge to the profile: build check, link, agent status, screen text, and Esc on a real blocked prompt |
| `plan.py` | The storyboard as data. Finds each focal ring in the source frame, plans the camera, spotlight and captions, and fails on a broken rule |
| `ink.py` | Finds text ink in a frame and measures the cap height of its lines |
| `scene.html`, `scene.mjs`, `camera.mjs` | The renderer that `docs/demo/render.mjs` steps frame by frame |
| `legibility.py` | Measures the focal text in the finished movie at README width, and writes the contact sheet and key frames |

## Before you start

You need the Namedrop testbed, Chromium, Node.js, ffmpeg, and Python 3 with Pillow and NumPy. Keep the take, renders and Python environment outside `/tmp`. A take is about 1 GB.

```sh
python3 -m venv ~/.cache/channels-video/venv
~/.cache/channels-video/venv/bin/pip install pillow numpy
```

## Capture a take

1. Build a `test-` profile from the product revision. The testbed links logins read-only and isolates `HOME` and `XDG_CONFIG_HOME`. Run the commands from the repository root.

   ```sh
   export TESTBED_HOME=~/.cache/channels-video/testbed
   testbed/testbed test --link . --profile test-channels --codex no-daemon --keep
   ```

2. Stage a clean workspace. Run this again before every take, because a take leaves messages in the channel.

   ```sh
   python3 docs/demo/channels/motion/stage.py test-channels
   ```

   Claude Code starts in auto mode, which runs the Needs-you command without asking. `stage.py` presses Shift-Tab until the pane shows `manual mode on`.

3. Start the recorder in one shell. It refuses a profile that is not `test-`, a product that differs from the revision in `profile.py`, and agents that are not registered and idle.

   ```sh
   CHROME=/usr/bin/chromium node docs/demo/channels/motion/record.mjs test-channels ~/.cache/channels-video/take
   ```

4. In another shell, queue the steps one at a time, and read each result before you queue the next:

   ```sh
   for step in docs/demo/channels/motion/steps/*.js; do
     node docs/demo/channels/motion/queue.mjs ~/.cache/channels-video/take "$step"
     until [ -f ~/.cache/channels-video/take/actions/$(basename "$step" .js).json.result ]; do sleep 1; done
     head -c 600 ~/.cache/channels-video/take/actions/$(basename "$step" .js).json.result; echo
   done
   ```

   The recorder skips a file name it has already run. To repeat a step, queue a copy under a new name. The last step stops the recorder.

Three steps need care:

- `5-needs.js` checks that Claude is really blocked before it records Needs you, then sends Esc to Claude's pane. It never approves the prompt. Afterwards, check that `release-notes.txt` does not exist in `~/release-planning`.
- `6-peek.js` sends the Type-to-agent text. Codex sometimes keeps that text as an unsent draft. The step then presses the Peek **Enter** control and records a `peek-extra-enter` event.
- Live Peek follows the bottom of the screen. Do not clear the Codex pane before the take. After a clear, the answer prints near the top of an empty screen and falls outside the expanded Peek view.

## Render the movie

1. Plan the cut. The planner prints the zoom, hold and focal cap height of every shot:

   ```sh
   ~/.cache/channels-video/venv/bin/python docs/demo/channels/motion/plan.py ~/.cache/channels-video/take --shotlist ~/.cache/channels-video/shotlist.md
   ```

   It fails when a push-in or hold is too short, a hold does not play at real speed, focal text is under 9.5 px tall at 900 px width, or the cut is not 30 to 40 seconds long. To change the cut, edit `SCENES` in `plan.py`. A `search` rectangle limits where a ring looks for ink. Use one for elements the recorder cannot measure, such as the reminder popover or the rows of the Peek terminal.

2. Serve the take with the scene files:

   ```sh
   cp docs/demo/channels/motion/scene.html docs/demo/channels/motion/scene.mjs docs/demo/channels/motion/camera.mjs ~/.cache/channels-video/take/
   python3 -m http.server 8772 --bind 127.0.0.1 --directory ~/.cache/channels-video/take
   ```

   Open `http://127.0.0.1:8772/scene.html?t=12` to preview from 12 seconds.

3. From another shell in the repository root, render and encode:

   ```sh
   SCENE_URL=http://127.0.0.1:8772/scene.html CHROME=/usr/bin/chromium RENDER_SCALE=1.3333333333333333 node docs/demo/render.mjs ~/.cache/channels-video/master.mp4 30
   ffmpeg -y -i ~/.cache/channels-video/master.mp4 -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p -an -movflags +faststart docs/assets/namedrop-channels.mp4
   ffmpeg -y -i docs/assets/namedrop-channels.mp4 -t 7.9 -vf 'fps=15,scale=1400:-1:flags=lanczos' -c:v libwebp_anim -quality 85 -compression_level 6 -loop 0 docs/assets/namedrop-channels.webp
   ```

   The WebP is the first scene. It ends where the second scene's dissolve starts, so the loop restarts on a clean boundary.

4. Check the result:

   ```sh
   ~/.cache/channels-video/venv/bin/python docs/demo/channels/motion/legibility.py ~/.cache/channels-video/take/timeline.json docs/assets/namedrop-channels.mp4 ~/.cache/channels-video/report
   python3 docs/demo/channels/check.py ~/.cache/channels-video/report/metadata.json --recording ~/.cache/channels-video/take/recording/recording.json
   ```

   `legibility.py` fails when a focal text line is under 9.5 px tall at 900 px width. It writes `legibility.md`, a crop of each ring, one 2560 px and one 900 px frame per shot, and `sheet.png` with a frame every two seconds. Look at the sheet and the frames before you publish. `check.py` checks the codec, size, length, fast start, WebP and the take's provenance. Update `capture.json` for the new take first.
