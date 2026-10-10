# Namedrop presentation

A narrated walkthrough of Namedrop in eight chapters, under three minutes long. A voice explains each feature while the page stages it like a product film: large type on black, one or two Herdr panes lit in their runtime color, and messages crossing between them as cards.

| File | Purpose |
|---|---|
| `script.mjs` | The narration. Each line has the words, the visual beat it cues, the on-screen headline (`head`), and any prompt or command the scene shows (`show`). |
| `voice.mjs` | Speaks `script.mjs` with the Qwen3-TTS voice model and writes `voice/<chapter>.mp3` and `timeline.json`. |
| `narrate.py` | Renders the lines `voice.mjs` has not cached yet, in one Python process, so the model loads once. |
| `narrator.flac` | The narrator's reference clip. Qwen3-TTS clones this voice for every line. |
| `check_voice.py` | Checks every line of the built narration: transcript, clipping, dropouts and endings. Reports pauses and speech rate. |
| `requirements.txt` | The Python packages of the Qwen3-TTS environment, at the versions the narration was built with. |
| `timeline.json` | The narration with each line's start and end time. The page loads it. |
| `index.html` | The player: start screen, hero type, captions, chapter index, seek bar, keyboard, and the audio clock. |
| `stage.js` | The beat registry and the Three.js stage. Each frame is a pure function of chapter and time. |
| `drv/shots.mjs` | Screenshots every beat at 1920x1080 and one beat per chapter at 390x844. |
| `drv/behave.mjs` | Drives the player in Chromium and checks the clock, seeking, the keyboard and the idle chrome. |
| `drv/film.mjs` | Records the narrated film and its poster for the repository README. |

## Watch it locally

```sh
cd docs/presentation
python3 -m http.server 8830 --bind 127.0.0.1
```

Open `http://127.0.0.1:8830/index.html`. Space plays and pauses, the arrow keys change chapter, C turns the captions on or off, and T opens the chapter index. Captions start off, and the page remembers your choice. During playback, the controls fade after 2.5 seconds without pointer movement, and a thin progress line stays. To inspect one frame, open `index.html?ch=<chapter-id>&t=<seconds>&still`.

## The narrator

The voice is synthetic. It is not a real person. Qwen3-TTS VoiceDesign rendered `narrator.flac` from the written description in `VOICE.description` in `script.mjs`. The [Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS) 1.7B Base model clones that clip for every line, using the clip's transcript (`VOICE.referenceText`).

Each line is spoken at the model's natural speed. When `voice.mjs` assembles the chapters, it speeds up every line by the same factor, `TEMPO` in `script.mjs` (1.08), with ffmpeg's pitch-preserving `atempo` filter. The gaps between lines are not stretched. Changing `TEMPO` re-renders nothing.

Rendering needs an NVIDIA GPU with about 6 GB of free memory, and Python 3.12. Set up the environment once:

```sh
python3.12 -m venv ~/.venvs/narrator
~/.venvs/narrator/bin/pip install -r requirements.txt
export NARRATOR_PYTHON=~/.venvs/narrator/bin/python
```

The first render downloads `Qwen/Qwen3-TTS-12Hz-1.7B-Base` from Hugging Face, about 4.3 GB. `check_voice.py` adds Whisper large-v3, about 3 GB. Rendering needs network access even after the download, because `qwen-tts` asks Hugging Face for the model's files every time it loads, so `HF_HUB_OFFLINE=1` makes it fail.

## Change the narration

1. Edit the lines in `script.mjs`. A line's `head` is set as large type. A beat can place it (`top`, `left`, `right`, `center`), mark words with a class, or reveal it sentence by sentence.
2. Run `node voice.mjs` with `NARRATOR_PYTHON` set. Each line is cached under a key made from its spoken text, its seed, `VOICE`, `narrator.flac` and `narrate.py`. A rerun renders only the lines whose key changed. Without `NARRATOR_PYTHON`, the build stops and reports how many lines need rendering.
3. Run `$NARRATOR_PYTHON check_voice.py` (see [Check the voice](#check-the-voice)). If a line fails, give it a `seed` other than `VOICE.seed` (for example `seed: 1`), run `node voice.mjs` again, and check that line again with `check_voice.py <beat>`. Use a per-line seed only for a line that fails a check.
4. If you added a beat, add an entry for it to `BEATS` in `stage.js`. The page logs a `console.error` for every beat in `timeline.json` that has no entry.

The voice settings are in `VOICE` in `script.mjs`. To change the voice, replace `narrator.flac` and set `referenceText` to the exact words spoken in it. Every line then renders again. Use only a voice you have the right to use.

## Check the voice

```sh
$NARRATOR_PYTHON check_voice.py            # every line; or name beats: check_voice.py shuttle peers
```

It cuts every line out of its chapter MP3 using the times in `timeline.json`. For each line it prints the duration, the speech rate in words per second, the longest internal pause, and `OK` or the problems. It exits with status 1 if any line fails. A line fails when:

- Whisper large-v3 hears different words than the spoken text. Case, punctuation and spacing are ignored, and so are homophones (`pane`/`pain`, `two`/`to`) and `at` before a word that starts with `t` (`at tester` sounds like `a tester`).
- Any sample is clipped.
- An internal pause is longer than 1.2 seconds (a dropout).
- The last 10 milliseconds are louder than a quarter of the line's average level, which means the line was cut off.

A pause longer than 0.8 seconds is noted on the row, but the line still passes. The speech rate is reported only. The last rows give the median and range. Whisper runs on the CPU, so a full check takes several minutes.

## Check the page

Both scripts need `playwright-core` resolvable from this directory (`npm install --no-save playwright-core`) and Chromium at `/usr/bin/chromium`, with the server above running.

```sh
node drv/shots.mjs /tmp/shots     # read every frame it writes
node drv/behave.mjs               # prints PASS/FAIL per check and the console log
```

## Record the film

GitHub can't run the page, so the repository README links a recorded copy, `docs/assets/namedrop-overview.mp4`. The script also writes a poster frame, `docs/assets/namedrop-overview.webp`. To record both again, with the server above running:

```sh
node drv/film.mjs ../assets/namedrop-overview.mp4 ../assets/namedrop-overview.webp
```

The script opens `index.html?film`. Film mode draws like `?still`, without the player chrome and captions, and the caret blink and the waiting pulse follow the film's clock instead of wall time. The script steps through the whole film at 30 fps with `window.renderAt`. It shoots each frame at 3840x2160 and scales it down to 1920x1080, which keeps the small pane text sharp. The soundtrack is the chapter MP3s, each cut or padded to its `duration` in `timeline.json`. The script raises the H.264 crf from 18 until the file fits `FILM_MAX_MB` (default 40), and fails if the video and audio lengths differ by more than one frame.

A recording takes about 25 minutes with a GPU. The script needs ffmpeg with libx264 and libwebp. It writes its intermediate files under `~/.cache/namedrop-film/` and deletes them at the end. To rewrite only the poster, set `FILM_ONLY=poster`.
