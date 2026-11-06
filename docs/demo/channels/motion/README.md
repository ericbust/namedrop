# Record and render the Channels highlight

The README movie is one continuous real take of the Channels page, cut into five short scenes. Each scene has one caption and one focal element at a time. The app is recorded at a 1280×720 CSS viewport with `deviceScaleFactor: 2`, so each source frame is 2560×1440. The movie uses the stage of the Namedrop demo in `docs/demo`: its background, windows, name plates, cards, beams and captions. The whole Channels window stays on the stage in every shot, and the camera does not move. Beside it are the real Codex and Claude Code windows, built from the agents' screens captured during the take, with name plates. A card quotes each real message as it travels between Channels and an agent, with a beam between the two windows. A spotlight ring marks the focal element in Channels, and a callout beside it repeats the element's key text in large type.

Only scaling, cuts, dissolves and overlays are added. The overlays are the captions, a spotlight that dims app pixels outside the focal ring, the ring itself, the callouts, the cards and beams, the **1 minute later** label and the end card. No app text is drawn or changed. Each callout and card copies text the page or an agent's screen really showed, and `plan.py` fails if it does not. The agent windows show only rows the agents printed. `plan.py` keeps the rows below the last delivered request, drops the rows of its JSON envelope and the rows with the testbed's private paths, and writes the profile's home as `~`, the same way `docs/demo/prep.py` does.

`capture.json` records the published take: product and source revisions, event times, and a hash of the source frame behind each focal hold.

| File | Purpose |
|---|---|
| `stage.py` | Builds a clean take in a `test-` profile: a new empty workspace named **Release planning**, both agents started in `~/release-planning`, warmed up and idle, and Claude in manual permission mode |
| `record.mjs` | Opens Channels in Chromium, records timestamped 2x frames and both agents' screens with colour, and runs queued step files. `mark()` stores each event with the measured focal element |
| `queue.mjs` | Queues one step file for the recorder |
| `steps/*.js` | The six capture steps, typed and sent through the real UI |
| `profile.py` | The only bridge to the profile: build check, link, agent status, screen text, and Esc on a real blocked prompt |
| `plan.py` | The storyboard as data. Finds each focal ring in the source frame, lays out the callouts, picks the agent screens and cards, writes `timeline.json` and `panes.js`, and fails on a broken rule |
| `ink.py` | Finds text ink in a frame and measures the cap height of its lines |
| `scene.html`, `scene.mjs` | The scene that `docs/demo/render.mjs` steps frame by frame. It builds on `docs/demo/stage3d.js` and `docs/demo/stage.css`, which the Namedrop demo shares |
| `legibility.py` | Measures the focal text in the app and in the callout in the finished movie at README width, and writes the contact sheet and key frames |

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

1. Plan the cut. The planner prints the hold, callout time and focal cap heights of every shot:

   ```sh
   ~/.cache/channels-video/venv/bin/python docs/demo/channels/motion/plan.py ~/.cache/channels-video/take --shotlist ~/.cache/channels-video/shotlist.md
   ```

   It fails when a hold is shorter than 2.5 s, a hold plays faster than real time, a callout or card is not text the take showed, a callout shows too briefly, the focal text is under 9.5 px tall at 900 px width both in the app and in the callout, or the cut is not 30 to 40 seconds long. It copies the callout font into the take directory, Liberation Sans Bold by default; pass `--font` to use another TrueType file. To change the cut, edit `SCENES` in `plan.py`. A `search` rectangle limits where a ring looks for ink. Use one for elements the recorder cannot measure, such as the reminder popover or the rows of the Peek terminal.

2. Serve the demo directory and the take side by side. The scene loads the take from `/take/`. Install the demo's packages once with `npm install` in `docs/demo`.

   ```sh
   mkdir -p ~/.cache/channels-video/serve
   ln -sfn "$PWD/docs/demo" ~/.cache/channels-video/serve/demo
   ln -sfn ~/.cache/channels-video/take ~/.cache/channels-video/serve/take
   python3 -m http.server 8773 --bind 127.0.0.1 --directory ~/.cache/channels-video/serve
   ```

   Open `http://127.0.0.1:8773/demo/channels/motion/scene.html?t=12` to preview from 12 seconds.

3. From another shell in the repository root, render and encode:

   ```sh
   SCENE_URL=http://127.0.0.1:8773/demo/channels/motion/scene.html CHROME=/usr/bin/chromium RENDER_SCALE=1.3333333333333333 node docs/demo/render.mjs ~/.cache/channels-video/master.mp4 30
   ffmpeg -y -i ~/.cache/channels-video/master.mp4 -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p -an -movflags +faststart docs/assets/namedrop-channels.mp4
   ffmpeg -y -i docs/assets/namedrop-channels.mp4 -t 7.8 -vf 'fps=15,scale=1400:-1:flags=lanczos' -c:v libwebp_anim -quality 85 -compression_level 6 -loop 0 docs/assets/namedrop-channels.webp
   ```

   The WebP is the first scene. It ends where the second scene's dissolve starts, so the loop restarts on a clean boundary.

4. Check the result:

   ```sh
   ~/.cache/channels-video/venv/bin/python docs/demo/channels/motion/legibility.py ~/.cache/channels-video/take/timeline.json docs/assets/namedrop-channels.mp4 ~/.cache/channels-video/report
   python3 docs/demo/channels/check.py ~/.cache/channels-video/report/metadata.json --recording ~/.cache/channels-video/take/recording/recording.json
   ```

   `legibility.py` fails when the focal text is under 9.5 px tall at 900 px width both inside the ring and in the callout. It writes `legibility.md`, a crop of each ring, one 2560 px and one 900 px frame per shot, and `sheet.png` with a frame every two seconds. Look at the sheet and the frames before you publish. `check.py` checks the codec, size, length, fast start, WebP and the take's provenance. Update `capture.json` for the new take first.
