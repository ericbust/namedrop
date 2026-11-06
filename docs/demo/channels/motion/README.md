# Record Channels with a moving camera

The published highlight uses the exp.17 product at `8ea5c1a`.
`capture.json` identifies the integrated build, real capture times and source hashes.
Every app frame is a 2x screenshot of the running page. Both terminal panels use
synchronized `herdr pane read --ansi` captures. No app text is redrawn.

The raw take stays outside Git because it is about 1.3 GB. Keep its `recording/`
directory with `recording.json`, `frames/` and `panes/`. The manifest records source
time, input and reply events, camera rectangles, the runtime version and Git source.
The edit follows that real sequence and compresses waiting. Terminal home paths are shortened to `~`, including wrapped paths.

## Capture a new take

Use `testbed/testbed` to build an owned `test-` profile from the product revision.
Keep HOME and XDG_CONFIG_HOME isolated. Finish startup before recording, and put the
demo repository inside the profile HOME so Peek uses short paths. Name the workspace
**Release planning**. Register and warm both real agents until they are idle. Add
real planning notes through the channel composer so close-ups include recent context.
Keep Claude in manual permission mode. Clear irrelevant terminal history before the take.

From the repository root, start the recorder:

```sh
TESTBED_HOME=/path/to/testbed CHROME=/path/to/chromium node docs/demo/channels/motion/record.mjs test-channels-exp17 /path/to/take
```

The helper rejects a non-test profile, a mismatched product tree or version, and
agents without a registered idle session. Chromium spellcheck is disabled through
browser flags. Set `HEADLESS=0` to watch the browser. Only enqueue your own trusted
JavaScript action files. The supplied steps type and send through the real UI:

```sh
for step in docs/demo/channels/motion/steps/*.js; do
  node docs/demo/channels/motion/queue.mjs /path/to/take "$step"
done
```

The recorder captures timestamped 2880 by 1800 frames and both real panes. It uses
a lower capture cadence during the actual two-minute reminder wait. Each action
writes a result beside its queued JSON. The permission step captures Needs you,
then sends Esc through a helper that requires a real blocked agent. It never
approves a permission. Verify the actual prompt and confirm no requested file exists.
Type to agent writes input first, then its Enter control submits it.

Inspect the take before rendering. Check aliases, registered status, reminder
Cancel and display text, and a Peek stream that stays live through its reply.
A DM-triggered permission showed stale status during this take, so the recorded
Needs-you beat was re-captured from the active workspace channel. The rejected
interval and evidence remain in the capture report. Esc also left an honest
unanswered-request notice in the captured app. Report these findings rather than
editing them out of source frames or claiming the product behavior is fixed.

## Render the take

`plan.py` holds the shot list. Each beat opens on the wide view of the app and both
panes, makes one eased push-in of about 1.3 seconds, then holds. Close-ups crop the
full app width at zoom 1.333 and keep its top or bottom. Beats change with a short
dissolve, and the two-minute reminder wait is a dissolve in time. The planner maps
output time to source time and fails when a move, hold, caption or total length is
outside its limits. `camera.mjs` evaluates the plan for both the scene and the
metrics. Generate the plan, then serve copied scene files beside the raw recording:

```sh
python3 docs/demo/channels/motion/plan.py /path/to/take --shotlist /path/to/report/shotlist.md
cp docs/demo/channels/motion/scene.html docs/demo/channels/motion/scene.mjs docs/demo/channels/motion/camera.mjs /path/to/take/
python3 -m http.server 8772 --bind 127.0.0.1 --directory /path/to/take
```

From another shell in the repository root:

```sh
SCENE_URL=http://127.0.0.1:8772/scene.html CHROME=/path/to/chromium RENDER_SCALE=1.3333333333333333 node docs/demo/render.mjs /path/to/master.mp4 24
ffmpeg -y -i /path/to/master.mp4 -c:v libx264 -preset slow -b:v 1800k -pix_fmt yuv420p -pass 1 -passlogfile /path/to/take/h264-pass -an -f null /dev/null
ffmpeg -y -i /path/to/master.mp4 -c:v libx264 -preset slow -b:v 1800k -pix_fmt yuv420p -pass 2 -passlogfile /path/to/take/h264-pass -an -movflags +faststart docs/assets/namedrop-channels.mp4
ffmpeg -y -i docs/assets/namedrop-channels.mp4 -t 14.3 -vf 'fps=12,scale=1400:-1:flags=lanczos' -c:v libwebp_anim -quality 65 -compression_level 6 -loop 0 docs/assets/namedrop-channels.webp
python3 docs/demo/channels/check.py /path/to/metadata.json --recording /path/to/take/recording/recording.json
node docs/demo/channels/motion/metrics.mjs new=/path/to/take/timeline.json,docs/assets/namedrop-channels.mp4
```

`metrics.mjs` reports camera moves per minute, peak zoom and pan speed, pan travel
while zoomed in, the shortest hold and caption, and a frame-difference check on the
movie. Pass an earlier timeline and movie as another column to compare cuts.

The poster is the opening channel beat, which loops on a beat boundary. Keep the MP4 under 15 MB and the poster
under 8 MB. Inspect exported frames at 900 px, especially the completed instructions,
agent work, pending reminder, fired answer, Needs you and live Peek reply. Keep the
capture report with timestamps, exact real messages, frame paths and omissions.

`rehearse.mjs` retains the earlier 28-second throwaway recorder. Its `throwaway`
marker excludes it from the final-media gate. The current planner does not map its
older event names.
