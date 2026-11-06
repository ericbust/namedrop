import json
import sys
from pathlib import Path
root=Path(sys.argv[1]).resolve()
m=json.loads((root/'recording/recording.json').read_text());events={e['name']:e['t'] for e in m['events']}
wide=[720,450,1,590];composer=[840,835,1.5,820];reply=[770,220,1.3,430];thread=[1090,830,1.65,820]
plan=[
 (0,2,'wide','type',wide,wide,'Your agents. One workspace.'),
 (2,4,'type','type',wide,composer,'Send an instruction.'),
 (4,9,'type','instruction',composer,composer,'Send an instruction.'),
 (9,11,'instruction','posted',composer,composer,'Send an instruction.'),
 (11,13,'posted','posted',composer,wide,'The agent gets to work.'),
 (13,15,'posted','reply',wide,wide,'The agent gets to work.'),
 (15,18,'reply','thread',wide,reply,'The reply lands here.'),
 (18,20,'thread','followup-type',reply,wide,'Open the thread.'),
 (20,22,'followup-type','followup',wide,thread,'Follow up without @.'),
 (22,25,'followup','followup-posted',thread,thread,'To @claude. No extra routing.'),
 (25,28,'followup-posted','end',thread,wide,'Back to the full workspace.'),
]
(root/'timeline.json').write_text(json.dumps([{'start':a,'end':b,'source':[events[x],events[y]],'from':c,'to':d,'caption':text} for a,b,x,y,c,d,text in plan],indent=2)+'\n')
print('Real source frames',len(m['frames']),'over',round(m['duration'],2),'seconds')
