# README demo

The animation at the top of the [README](../../README.md) is built from real sessions. Every terminal screen in it is output captured from Claude Code, Codex, a shell and Herdr's own popups, and every message card quotes what an agent actually sent, cut short with an ellipsis. `scene.html` restages those screens in 3D with Three.js. Nothing on screen is typed or edited by hand.

It tells two stories. First a lead hands work to `@reviewer` and `@tester` and merges their replies. Then `@coder` in Codex and `@tester` in Claude Code get their own jobs and work as peers, one conversation at a time. Each message crosses between two panes as a card. `@coder` asks `@reviewer` in Claude Code for a pinned review, and `@tester` runs its tests on `@build`, a plain shell. When the code moves, the review goes stale, and then it comes back fresh. Name plates and beams carry each runtime's color.

| File | Purpose |
|---|---|
| `frames.js` | The captured screens used by the scene, as ANSI rows |
| `stage3d.js`, `stage.css` | The stage both README movies share: background, windows, name plates, cards, beams and captions |
| `scene.html`, `scene.js` | The scene: CSS3D windows for the terminals, message cards and name plates, a WebGL layer for the background and handoff beams, and a deterministic timeline |
| `render.mjs` | Steps the timeline frame by frame in headless Chrome and pipes the frames to ffmpeg |
| `stills.mjs` | Renders the Agent status still and the social preview at 2x |
| `prep.py` | Crops raw captures into `frames.js` |
| `capture.sh` | Snapshots every pane in a workspace every few seconds while agents work |
| `vt.py` | A headless terminal that runs a real Herdr client, so popups can be captured |

## Re-render

You need Google Chrome, Node.js, ffmpeg and Python 3 with Pillow.

```sh
cd docs/demo
npm install
python3 -m http.server 8765 --bind 127.0.0.1 &
open 'http://127.0.0.1:8765/scene.html?t=12'   # live preview, starting at 12s
node render.mjs master.mp4                      # 3840×2160, rendered at 2x for crisp text, 30 fps
node stills.mjs                                 # agent-status.png, social-2x.png
```

Encode the published assets:

```sh
ffmpeg -i master.mp4 -vf "scale=2560:-2:flags=lanczos" -c:v libx264 -preset slow -crf 22 -pix_fmt yuv420p -movflags +faststart ../assets/namedrop-demo.mp4
mkdir -p webp && ffmpeg -i master.mp4 -vf "fps=15,scale=1400:-1:flags=lanczos" webp/%04d.png
python3 -c 'import glob; from PIL import Image
f = [Image.open(p).convert("RGB") for p in sorted(glob.glob("webp/*.png"))]
f[0].save("../assets/namedrop-demo.webp", save_all=True, append_images=f[1:], duration=66, loop=0,
          quality=75, method=6, minimize_size=True, allow_mixed=True)'
ffmpeg -i agent-status.png -vf "scale=1600:-1:flags=lanczos" ../assets/agent-status.png
ffmpeg -i social-2x.png -vf "scale=1280:640:flags=lanczos" ../assets/social-preview.png
```

The WebP step takes about twelve minutes. `social-preview.png` is for the repository's **Settings → Social preview**. The renderer keeps the background still, because an animated gradient makes every frame differ and inflates the WebP.

## Capture new screens

The screens come from an isolated profile under `~/namedrop-demo`, so no personal workspace, history or sidebar appears in them. Keep it outside `/tmp`, because macOS clears `/private/tmp` on restart.

1. Give the profile its own `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, `CODEX_HOME` and a short `HERDR_SOCKET_PATH` (macOS limits socket paths to 104 bytes), in an `env.sh` that also sets `RUSTUP_HOME` and `CARGO_HOME` back to your own toolchain so `cargo` still works. Start `herdr server` from a clean environment with it. A shell inside another Herdr or Claude Code session carries `HERDR_*` and `CLAUDE*` variables that must not leak into the demo.
2. On macOS, Claude Code in a new `HOME` looks for your login keychain at `$HOME/Library/Keychains`; symlink it to `~/Library/Keychains` to reuse your existing login instead of creating a new one. Codex reads `$CODEX_HOME/auth.json`, so run `codex login` inside the profile, or link that file to your existing one.
3. Clone this repository into the profile and create the workspace the agents will work in with `herdr worktree create --cwd <clone> --branch <name> --base <commit> --path <dir>`. Pinned reviews and freshness checks need a workspace backed by a Git worktree, and a base before the fix you want to show keeps the tests honest.
4. Run `sh <clone>/scripts/setup.sh` from a pane in that workspace, as a new user would, then `herdr integration install claude` and `herdr integration install codex` if setup reports Herdr's native hooks as missing. Start `codex` once and trust the Namedrop and Herdr hooks in `/hooks`. Start `claude` once and finish its first-run prompts.
5. Start agents in separate tabs and name them. Record the whole session with `sh capture.sh <workspace-id> <out-dir>`, which saves every pane as `<epoch>-<pane>.ansi` every two seconds, and save one-off moments, such as a typed prompt before Enter, with `herdr pane read <pane> --source visible --ansi`.
6. Popups are drawn by the Herdr client, not stored in a pane. Run `python3 vt.py 160 46 vt.fifo -- herdr` with the profile's environment to attach a client in a virtual terminal, send keys with `printf 'keys "\u0002"\n' > vt.fifo` for the default Ctrl-B prefix (use your own prefix if it differs), and save the screen with `printf 'snap out.ansi\n' > vt.fifo`. `vt.py` needs `pip install pyte`.
7. Point the `FRAMES` table in `prep.py` at the new captures and run `python3 prep.py <capture-dir>`.

`prep.py` only picks rows. It drops Claude Code's grey, unsent prompt suggestions and its one-time notice, collapses long blank gaps, blanks the stray keys the virtual terminal leaves in a popup capture's input box, shortens the profile's home path to `~`, and maps the virtual terminal's stock ANSI colors to Herdr's palette.

## Re-capture and render Channels

The Channels movie is a silent keynote film built from one continuous take of the real Channels page on the 1.5.0-exp.20 build, recorded at a 1280×720 viewport at 2x. Each beat has one headline and shows one idea for four to six seconds: the full app, asking an agent, following up in the thread without @, Claude asking Codex in the open, **All activity**, picking who gets a reply, skills from the composer, a reminder in a DM, Needs you, subagents on the member chip, and a closing card with `namedrop channels`. The app appears as large rounded crops of recorded frames. The camera does not move. Only type, glows, cards, the **1 minute later** label, a ring around Needs you, and motion are added; no app text is drawn or changed. The movie is 50 to 60 seconds long, and the animated WebP is its first nine seconds. The person in the movie is "you", and `wordcheck.py` fails the render if the word "operator" appears anywhere on screen.

| Path | Purpose |
|---|---|
| `channels/record/stage.py` | Builds a clean take in a `test-` profile: an empty workspace named **Release planning**, both agents started in `~/release-planning`, idle, and Claude in manual permission mode |
| `channels/record/record.mjs`, `queue.mjs` | Opens Channels in Chromium, records timestamped 2x frames, and runs queued step files |
| `channels/record/steps/*.js` | The capture steps, typed and sent through the real UI |
| `channels/record/profile.py` | The only bridge to the profile: build check, link, agent status, screen text, and Esc on a real blocked prompt |
| `channels/keynote/scene.html` | The film. Each scene picks recorded frames by their time in the take |
| `channels/keynote/render.mjs` | Serves the scene and the take, and steps the film frame by frame into ffmpeg at 2560×1440 |
| `channels/keynote/capture.json` | The published take's build, source revisions and event times |
| `channels/keynote/check.py`, `wordcheck.py` | Check the encoded assets against the take, and read every half second of the movie for the banned word |
| `channels/keynote/fonts/` | Inter Display, under the SIL Open Font License in `LICENSE.txt` |

### Capture a take

You need the Namedrop testbed, Chromium, Node.js, ffmpeg, and Python 3 with Pillow. Keep the take and renders outside `/tmp`; a take is about 1 GB. Run the commands from the repository root.

1. Build a `test-` profile from the product revision. The testbed isolates `HOME` and `XDG_CONFIG_HOME` and links your existing login files.

   ```sh
   export TESTBED_HOME=~/.cache/channels-video/testbed
   testbed/testbed test --link . --profile test-channels --codex no-daemon --keep
   ```

2. Stage a clean workspace before every take. Claude Code starts in auto mode, which would run the Needs-you command without asking, so `stage.py` presses Shift-Tab until the pane shows `manual mode on`. It also tells the agents not to send a thank-you after another agent answers, because a thank-you tends to name you by your Namedrop alias.

   ```sh
   python3 docs/demo/channels/record/stage.py test-channels
   ```

3. Start the recorder. It refuses a profile that is not `test-`, a product that differs from the revision in `profile.py`, and agents that are not registered and idle.

   ```sh
   CHROME=/usr/bin/chromium node docs/demo/channels/record/record.mjs test-channels ~/.cache/channels-video/take
   ```

4. In another shell, queue the steps one at a time and read each result before you queue the next. The last step stops the recorder.

   ```sh
   steps_dir=docs/demo/channels/record/steps
   for step in $(cd "$steps_dir" && printf '%s\n' *.js | sort -n); do
     node docs/demo/channels/record/queue.mjs ~/.cache/channels-video/take "$steps_dir/$step"
     until [ -f ~/.cache/channels-video/take/actions/$(basename "$step" .js).json.result ]; do sleep 1; done
     head -c 600 ~/.cache/channels-video/take/actions/$(basename "$step" .js).json.result; echo
   done
   ```

   `3-collab.js` fails if Claude finishes without bringing in @codex; keep its wording, because Claude routes to @codex only when the reply is addressed to both. `6-reminder.js` gives Claude the reminder's exact text. `8-needs.js` checks that Claude is really blocked, then sends Esc and never approves the prompt; afterwards, check that `release-notes.txt` does not exist in `~/release-planning`. `9-subagents.js` waits for the subagent count on Claude's member chip; if a subagent stops on a prompt, send Esc with `profile.py cancel`.

A new take has new timings. Update `capture.json` from the take's `recording/recording.json`, and update the take times in `scene.html` (each scene names the moments it uses) before you render.

### Render

Install the demo's packages once with `npm install` in `docs/demo`; the renderer uses its `playwright-core`.

```sh
cd docs/demo/channels/keynote
CHROME=/usr/bin/chromium node render.mjs ~/.cache/channels-video/take --stills 6.5,22,47 a.png,b.png,c.png   # preview frames
CHROME=/usr/bin/chromium node render.mjs ~/.cache/channels-video/take ~/.cache/channels-video/master.mp4
ffmpeg -y -i ~/.cache/channels-video/master.mp4 -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p -an -movflags +faststart ../../../assets/namedrop-channels.mp4
ffmpeg -y -i ~/.cache/channels-video/master.mp4 -t 9 -vf 'fps=15,scale=1400:-1:flags=lanczos' -c:v libwebp_anim -quality 80 -compression_level 6 -loop 0 ../../../assets/namedrop-channels.webp
python3 check.py ~/.cache/channels-video/take ../../../assets/namedrop-channels.mp4 ../../../assets/namedrop-channels.webp
python3 wordcheck.py ../../../assets/namedrop-channels.mp4 ~/.cache/channels-video/wordcheck.md
```

On macOS, leave out `CHROME` to use Google Chrome. `wordcheck.py` needs Tesseract. If a real agent message says the banned word, take that step again. Look at a frame every two seconds before you publish.
