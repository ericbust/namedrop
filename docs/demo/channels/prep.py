"""Crop real 2x captures into readable scene windows. No text is repainted."""
import json
from pathlib import Path
import subprocess

root=Path(__file__).resolve().parent
crops={
 'ask':('02-ask.png',(260,805,1410,890)),
 'channel':('03-channel-reply.png',(260,160,1410,340)),
 'thread':('04-followup.png',(1045,55,1430,310)),
 'followup':('04-followup.png',(1045,805,1430,890)),
 'followup-result':('04-followup-result.png',(1045,310,1430,465)),
 'dm-request':('06-reminder-pending.png',(270,440,1390,580)),
 'dm-pending':('06-reminder-pending.png',(1015,40,1380,168)),
 'dm-fired':('07-reminder-fired.png',(270,660,1120,770)),
 'peek-reply':('09-peek.png',(1045,395,1425,625)),
 'peek-input':('09-peek-input.png',(1045,745,1425,895)),
}
(root/'crops').mkdir(exist_ok=True)
manifest={}
for name,(source,box) in crops.items():
 x,y,right,bottom=box
 subprocess.run(['ffmpeg','-y','-loglevel','error','-i',str(root/'captures'/source),'-vf',f'crop={(right-x)*2}:{(bottom-y)*2}:{x*2}:{y*2}',str(root/'crops'/f'{name}.png')],check=True)
 manifest[name]={'source':source,'cssBox':box,'width':box[2]-box[0],'height':box[3]-box[1]}
(root/'crops.json').write_text(json.dumps(manifest,indent=2)+'\n')
