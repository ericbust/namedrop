"""Check the published Channels movie and WebP, and that the take matches capture.json.

python3 check.py <take-dir> <namedrop-channels.mp4> <namedrop-channels.webp>"""
import json
from pathlib import Path
import subprocess
import sys

take, movie, webp = map(Path, sys.argv[1:4])
problems = []
def need(ok, message):
    if not ok: problems.append(message)

probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-print_format', 'json', '-show_streams', '-show_format', str(movie)]))
video = next(s for s in probe['streams'] if s['codec_type'] == 'video')
duration, size = float(probe['format']['duration']), movie.stat().st_size
need(video['codec_name'] == 'h264' and video['pix_fmt'] == 'yuv420p', f"movie is {video['codec_name']} {video['pix_fmt']}, want h264 yuv420p")
need((video['width'], video['height']) == (2560, 1440), f"movie is {video['width']}x{video['height']}, want 2560x1440")
need(50 <= duration <= 60, f'movie is {duration:.1f} s, want 50 to 60 s')
need(size < 15e6, f'movie is {size / 1e6:.1f} MB, want under 15 MB')
head = movie.read_bytes()[:1 << 20]
need(0 <= head.find(b'moov') < head.find(b'mdat'), 'movie is not fast start: moov must come before mdat')

data = webp.read_bytes()
need(data[:4] == b'RIFF' and data[8:12] == b'WEBP' and b'ANMF' in data, 'WebP is not an animated WebP')
width = 1 + int.from_bytes(data[data.find(b'VP8X') + 12:][:3], 'little')
need(width == 1400, f'WebP is {width} px wide, want 1400')
need(len(data) < 8e6, f'WebP is {len(data) / 1e6:.1f} MB, want under 8 MB')

capture = json.loads(Path(__file__).with_name('capture.json').read_text())
recording = json.loads((take / 'recording' / 'recording.json').read_text())
for key in ('productRevision', 'sourceRevision', 'version', 'startedAtEpochMs'):
    need(recording[key] == capture[key], f'take {key} {recording[key]} differs from capture.json {capture[key]}')
need(len(recording['frames']) == capture['sourceFrameCount'], 'take frame count differs from capture.json')

print(f'movie: {video["width"]}x{video["height"]} {video["codec_name"]}, {duration:.1f} s, {size / 1e6:.1f} MB')
print(f'webp: {width} px, {len(data) / 1e6:.1f} MB')
print('\n'.join(problems) or 'check passed')
sys.exit(1 if problems else 0)
