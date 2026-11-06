"""Verify the published Channels movie, animated excerpt and capture provenance."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('report',nargs='?',type=Path)
parser.add_argument('--recording',type=Path)
args=parser.parse_args()
root=Path(__file__).resolve().parent
assets=root.parent.parent/'assets'
mp4=assets/'namedrop-channels.mp4';webp=assets/'namedrop-channels.webp'
provenance=json.loads((root/'motion/capture.json').read_text())
assert provenance['productRevision']=='8ea5c1a21d61da0e2539a2d8cae0638557c83dab','expected exp.17 product'
if args.recording:
 recording=json.loads(args.recording.read_text())
 assert recording['throwaway'] is False,'rehearsal footage cannot be published'
 assert recording['productRevision']==provenance['productRevision'],'wrong product revision'
 assert recording['sourceRevision']==provenance['sourceRevision'],'wrong source build'
 assert recording['version']=='1.5.0-exp.17','wrong runtime version'
 assert recording['viewport']=={'width':1440,'height':900} and recording['deviceScaleFactor']==2,'capture must be 2x'
 assert len(recording['panes'])>1,'real panes required'
 events={event['name']:event for event in recording['events']}
 assert events['reminder-fired']['t']-events['reminder-pending']['t']>=110,'reminder must really wait two minutes'
 assert not recording['errors'],recording['errors']
 for source in provenance['sourceFrames']:
  assert hashlib.sha256((args.recording.parent/source['file']).read_bytes()).hexdigest()==source['sha256'],source['file']
probe=json.loads(subprocess.check_output(['ffprobe','-v','error','-show_format','-show_streams','-of','json',str(mp4)]))
streams=probe['streams'];duration=float(probe['format']['duration'])
assert len(streams)==1 and streams[0]['codec_name']=='h264','silent H.264 required'
assert streams[0]['width']==2560 and streams[0]['height']==1440,'wrong video dimensions'
assert 45<=duration<=60 and abs(duration-provenance['outputDuration'])<0.05,'wrong movie duration'
assert mp4.stat().st_size<15_000_000,'MP4 too large'
video=mp4.read_bytes();assert 0<video.find(b'moov')<video.find(b'mdat'),'fast-start MP4 required'
data=webp.read_bytes();assert data[:4]==b'RIFF' and data[8:12]==b'WEBP','not WebP'
offset=12;durations=[];width=None
while offset+8<=len(data):
 kind=data[offset:offset+4];size=int.from_bytes(data[offset+4:offset+8],'little');payload=data[offset+8:offset+8+size]
 if kind==b'VP8X':width=1+int.from_bytes(payload[4:7],'little')
 if kind==b'ANMF':durations.append(int.from_bytes(payload[12:15],'little'))
 offset+=8+size+(size%2)
assert width==1400 and len(durations)>1,'1400px animated poster required'
assert 5_000<=sum(durations)<=duration*1000+100,'invalid poster excerpt duration'
assert webp.stat().st_size<8_000_000,'poster too large'
readme=(root.parents[2]/'README.md').read_text();assert '](docs/assets/namedrop-channels.mp4)' in readme,'README movie link missing'
result={'mp4':{'bytes':mp4.stat().st_size,'durationSeconds':duration,'width':2560,'height':1440,'codec':'h264','audioStreams':0,'faststart':True,'sha256':hashlib.sha256(video).hexdigest()},'webp':{'bytes':webp.stat().st_size,'durationSeconds':sum(durations)/1000,'width':width,'frames':len(durations),'sha256':hashlib.sha256(data).hexdigest()},'productRevision':provenance['productRevision'],'sourceRevision':provenance['sourceRevision'],'captureVerified':bool(args.recording)}
text=json.dumps(result,indent=2)+'\n';print(text,end='')
if args.report:args.report.write_text(text)
