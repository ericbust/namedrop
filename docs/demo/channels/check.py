"""Check the published Channels media and its real-capture sources."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

root=Path(__file__).resolve().parent
assets=root.parent.parent/'assets'
mp4=assets/'namedrop-channels.mp4'
webp=assets/'namedrop-channels.webp'
manifest=json.loads((root/'captures.json').read_text())
for capture in manifest['captures']:
 assert hashlib.sha256((root/'captures'/capture['file']).read_bytes()).hexdigest()==capture['sha256'],capture['file']
probe=json.loads(subprocess.check_output(['ffprobe','-v','error','-show_format','-show_streams','-of','json',str(mp4)]))
streams=probe['streams'];assert len(streams)==1 and streams[0]['codec_name']=='h264',streams
assert streams[0]['width']==2560,streams[0]['width']
assert abs(float(probe['format']['duration'])-40)<0.05,probe['format']['duration']
assert mp4.stat().st_size<15_000_000,mp4.stat().st_size
video=mp4.read_bytes();assert 0<video.find(b'moov')<video.find(b'mdat'),'MP4 must start before download completes'
data=webp.read_bytes();assert data[:4]==b'RIFF' and data[8:12]==b'WEBP','not WebP'
offset=12;durations=[];width=None
while offset+8<=len(data):
 kind=data[offset:offset+4];size=int.from_bytes(data[offset+4:offset+8],'little');payload=data[offset+8:offset+8+size]
 if kind==b'VP8X':width=1+int.from_bytes(payload[4:7],'little')
 if kind==b'ANMF':durations.append(int.from_bytes(payload[12:15],'little'))
 offset+=8+size+(size%2)
assert width==1400,width
assert len(durations)>1,'poster must animate'
assert abs(sum(durations)-40_000)<100,(sum(durations),len(durations))
assert webp.stat().st_size<8_000_000,webp.stat().st_size
readme=(root.parents[2]/'README.md').read_text();assert '](docs/assets/namedrop-channels.mp4)' in readme
result={'mp4':{'bytes':mp4.stat().st_size,'durationSeconds':float(probe['format']['duration']),'width':2560,'codec':'h264','audioStreams':0,'faststart':True},'webp':{'bytes':webp.stat().st_size,'durationSeconds':sum(durations)/1000,'width':width,'frames':len(durations)},'captureHashes':'all match'}
text=json.dumps(result,indent=2)+'\n';print(text,end='')
if len(sys.argv)>1:Path(sys.argv[1]).write_text(text)
