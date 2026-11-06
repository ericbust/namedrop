# README demo

The animation at the top of the [README](../../README.md) is built from real sessions. Every terminal screen in it is output captured from Claude Code, Codex, a shell and Herdr's own popups, and every message card quotes what an agent actually sent, cut short with an ellipsis. `scene.html` restages those screens in 3D with Three.js. Nothing on screen is typed or edited by hand.

It tells two stories. First a lead hands work to `@reviewer` and `@tester` and merges their replies. Then `@coder` in Codex and `@tester` in Claude Code get their own jobs and work as peers, one conversation at a time. Each message crosses between two panes as a card. `@coder` asks `@reviewer` in Claude Code for a pinned review, and `@tester` runs its tests on `@build`, a plain shell. When the code moves, the review goes stale, and then it comes back fresh. Name plates and beams carry each runtime's color.

| File | Purpose |
|---|---|
| `frames.js` | The captured screens used by the scene, as ANSI rows |
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

The 40-second Channels highlight uses real Claude Code and Codex sessions in one
isolated `test-channels-video` profile. Its source screenshots are in
`channels/captures/`. `channels/prep.py` crops those screenshots into larger windows
for README playback. It does not paint text or alter the page. `channels/terminals.json`
contains cropped rows from real `herdr pane read --ansi` captures.

From the repository root, check the embedded app and start an isolated profile:

```sh
(cd experimental/channels && npm ci && npm run bundle:check)
TESTBED_HOME="$PWD/../testbed" testbed/testbed up --link "$PWD" --profile test-channels-video
```

Use the profile's generated `env.sh` and the testbed wrapper in the
[Channels browser driver](../../.claude/skills/verify/SKILL.md#channels-browser-app).
Keep HOME and XDG_CONFIG_HOME isolated. Use only the testbed socket. Rename the
workspace to **Release planning**, finish the recognized onboarding screens, and
warm both agents with a harmless prompt. Open the embedded Channels app through
the profile's `namedrop channels --no-open --json`. Keep its nonce URL private.

Run `CHROME=/path/to/chromium node docs/demo/channels/capture.mjs "$CHANNELS_URL" /path/to/captures`
from the repository root. Drive the real page in that browser and type capture names
in the terminal. It saves 1440 by 900 screenshots at 2x. Record the channel request,
its reply, a one-agent thread follow-up with **To @codex**, a Claude DM reminder with
**Cancel** visible, the reminder's reply after two minutes, and **Type to agent** in
Live Peek. **Send** writes the text into the agent pane. The **Enter** key control
submits it. Cancel or deny every tool approval used for a Needs-you capture.

Replace the corresponding source PNGs, check the crop boxes in `channels/prep.py`,
and save a timestamp and quoted real message for each capture. Check the sidebar
aliases, status chips, and Peek frame before rendering. The current cut omits
Needs you because its DM sidebar said "working" while the member chip said
"Needs you". The captured Peek reply required reopening the drawer after input
reported a changed pane. These are capture findings for follow-up, not simulated
success states.

From `docs/demo`, render with the shared deterministic renderer:

```sh
python3 channels/prep.py
python3 -m http.server 8767 --bind 127.0.0.1 &
SCENE_URL=http://127.0.0.1:8767/channels/scene.html CHROME=/path/to/chromium RENDER_SCALE=1.3333333333333333 node render.mjs /path/to/channels-master.mp4 24
ffmpeg -y -i /path/to/channels-master.mp4 -c:v libx264 -preset slow -crf 22 -pix_fmt yuv420p -movflags +faststart ../assets/namedrop-channels.mp4
ffmpeg -y -i ../assets/namedrop-channels.mp4 -vf 'fps=12,scale=1400:-1:flags=lanczos' -c:v libwebp_anim -quality 75 -compression_level 6 -loop 0 ../assets/namedrop-channels.webp
```

Keep builds and renders outside `/tmp`. Inspect one exported frame per beat at
900 px width, and confirm both asset durations and sizes before updating the README.

Run `python3 channels/check.py /path/to/metadata.json` to verify capture hashes,
H.264 with no audio, fast-start MP4 layout, both widths, the 40-second duration,
and the asset size limits. It reads animated WebP frame durations directly.

Run `CHROME=/path/to/chromium node channels/check-scene.mjs /path/to/preview-frames`
to render each beat and check that seeking back reproduces the overview.
