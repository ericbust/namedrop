import difflib
import json
import re
import subprocess
import sys
from pathlib import Path

import numpy as np
from faster_whisper import WhisperModel

HERE = Path(__file__).parent
SR = 24000
FRAME_S = 0.01
SILENCE_BELOW_RMS_DB = 24.0
DROPOUT_S = 1.2
LONG_PAUSE_S = 0.8
HOMOPHONES = {"two": "to", "too": "to", "pain": "pane", "clawed": "claude"}


def words(text):
    return re.findall(r"[a-z0-9']+", text.lower())


def canonical(ws):
    """The two t's in "at tester" merge into one sound, so "at tester" and "a tester" are the same audio."""
    ws = [HOMOPHONES.get(w, w) for w in ws]
    return ["a" if w == "at" and nxt.startswith("t") else w for w, nxt in zip(ws, ws[1:] + [""])]


def silences(x):
    n = int(FRAME_S * SR)
    frames = x[: len(x) // n * n].reshape(-1, n)
    level = 20 * np.log10((np.sqrt((frames ** 2).mean(1)) + 1e-12) / (np.sqrt((x ** 2).mean()) + 1e-12))
    runs, start = [], None
    for i, silent in enumerate(np.append(level < -SILENCE_BELOW_RMS_DB, False)):
        if silent and start is None:
            start = i
        elif not silent and start is not None:
            runs.append((start * FRAME_S, i * FRAME_S))
            start = None
    return runs


def pace(x, text):
    dur, runs = len(x) / SR, silences(x)
    lead = runs[0][1] if runs and runs[0][0] == 0 else 0.0
    trail = dur - runs[-1][0] if runs and runs[-1][1] >= dur - FRAME_S else 0.0
    pauses = [b - a for a, b in runs if a > 0 and b < dur - FRAME_S]
    return len(words(text)) / (dur - lead - trail), max(pauses, default=0.0)


def decode(path, sr):
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-ac", "1", "-ar", str(sr), "-"],
                         capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def transcript_errors(model, x16, text):
    """Half a second of padding keeps Whisper from dropping a first word that starts right at the cut."""
    pad = np.zeros(8000, dtype=np.float32)
    segments, _ = model.transcribe(np.concatenate([pad, x16, pad]), language="en", beam_size=5)
    heard = " ".join(s.text.strip() for s in segments)
    if "".join(words(text)) == "".join(words(heard)):
        return heard, []
    ref, hyp = canonical(words(text)), canonical(words(heard))
    if "".join(ref) == "".join(hyp):
        return heard, []
    ops = difflib.SequenceMatcher(a=ref, b=hyp).get_opcodes()
    return heard, [f"{' '.join(ref[a:b])!r} -> {' '.join(hyp[c:d])!r}" for op, a, b, c, d in ops if op != "equal"]


def line_report(model, x, x16, text):
    rate, pause = pace(x, text)
    ending = np.sqrt((x[-int(0.01 * SR):] ** 2).mean()) / (np.sqrt((x ** 2).mean()) + 1e-12)
    failures, notes = [], []
    if (clipped := int((np.abs(x) >= 0.999).sum())):
        failures.append(f"{clipped} clipped samples")
    if pause > DROPOUT_S:
        failures.append(f"dropout {pause:.2f}s")
    elif pause > LONG_PAUSE_S:
        notes.append(f"long pause {pause:.2f}s")
    if ending > 0.25:
        failures.append(f"truncated ending ({ending:.2f} of line RMS in the last 10 ms)")
    heard, diff = transcript_errors(model, x16, text)
    if diff:
        failures.append(f"transcript {'; '.join(diff)} (heard: {heard})")
    return failures, notes, rate, pause


def main():
    only = set(sys.argv[1:])
    timeline = json.loads((HERE / "timeline.json").read_text())
    model = WhisperModel("large-v3", device="cpu", compute_type="int8")
    print(f"fail: transcript, clipping, pause over {DROPOUT_S}s, truncated ending. note: pause over {LONG_PAUSE_S}s")
    failed, rates = 0, []
    for chapter in timeline["chapters"]:
        audio, audio16 = decode(HERE / chapter["audio"], SR), decode(HERE / chapter["audio"], 16000)
        for line in chapter["lines"]:
            if only and line["beat"] not in only:
                continue
            x = audio[int(line["start"] * SR) : int(line["end"] * SR)]
            x16 = audio16[int(line["start"] * 16000) : int(line["end"] * 16000)]
            failures, notes, rate, pause = line_report(model, x, x16, line.get("spoken", line["say"]))
            failed += bool(failures)
            rates.append(rate)
            verdict = "; ".join(failures) if failures else "OK"
            print(f"{chapter['id']:<10} {line['beat']:<16} {len(x) / SR:5.2f}s {rate:4.2f} w/s  pause {pause:4.2f}s  "
                  f"{verdict}{''.join(f'  (note: {n})' for n in notes)}", flush=True)
    print(f"rate: median {np.median(rates):.2f} w/s, range {min(rates):.2f}-{max(rates):.2f}")
    print(f"{failed} line(s) failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
