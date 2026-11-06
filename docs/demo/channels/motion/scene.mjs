const W=1920,H=1080,canvas=document.querySelector('canvas'),ratio=devicePixelRatio;
canvas.width=Math.round(W*ratio);canvas.height=Math.round(H*ratio);
const ctx=canvas.getContext('2d');ctx.scale(ratio,ratio);
const params=new URLSearchParams(location.search);
const recordingBase=params.get('recording')||'recording';
const timelineURL=params.get('timeline')||'timeline.json';
let recording,timeline;
const cache=new Map();
async function frame(file){
 if(!cache.has(file)){const image=new Image();image.src=recordingBase+'/'+file;await image.decode();cache.set(file,image);if(cache.size>4)cache.delete(cache.keys().next().value);}
 return cache.get(file);
}
const ease=x=>x*x*(3-2*x);
function interpolate(a,b,p){return a.map((v,i)=>v+(b[i]-v)*ease(p));}
window.sceneReady=(async()=>{recording=await(await fetch(recordingBase+'/recording.json')).json();timeline=await(await fetch(timelineURL)).json();window.DURATION=timeline.at(-1).end;await frame(recording.frames[0].file);})();
window.renderAt=async t=>{
 await window.sceneReady;
 const section=timeline.find(s=>t>=s.start&&t<s.end)||timeline.at(-1);
 const p=Math.max(0,Math.min(1,(t-section.start)/(section.end-section.start)));
 const sourceTime=section.source[0]+p*(section.source[1]-section.source[0]);
 let low=0,high=recording.frames.length-1;while(low<high){const mid=Math.ceil((low+high)/2);if(recording.frames[mid].t<=sourceTime)low=mid;else high=mid-1;}
 const image=await frame(recording.frames[low].file),camera=interpolate(section.from,section.to,p),[cx,cy,zoom,anchorY]=camera;
 ctx.fillStyle='#060609';ctx.fillRect(0,0,W,H);
 const originX=W/2-cx*zoom,originY=anchorY-cy*zoom;
 ctx.save();ctx.beginPath();ctx.rect(0,130,W,H-130);ctx.clip();ctx.drawImage(image,originX,originY,1440*zoom,900*zoom);ctx.restore();
 ctx.fillStyle='#f5f5f7';ctx.textAlign='center';ctx.font='700 44px Arial';ctx.fillText(section.caption,W/2,63);
 ctx.fillStyle='#9999a5';ctx.font='23px Arial';ctx.fillText('Motion pipeline rehearsal · old build · real sessions',W/2,103);
 const overviewAlpha=Math.max(0,Math.min(1,(zoom-1)/.2));
 if(overviewAlpha>0){
  ctx.save();ctx.globalAlpha=overviewAlpha;
  const mw=370,mh=mw*900/1440,mx=W-mw-30,my=150;
  ctx.fillStyle='#111118';ctx.fillRect(mx-6,my-6,mw+12,mh+12);ctx.drawImage(image,mx,my,mw,mh);
  const left=Math.max(0,-originX/zoom),top=Math.max(0,(130-originY)/zoom),right=Math.min(1440,(W-originX)/zoom),bottom=Math.min(900,(H-originY)/zoom);
  ctx.strokeStyle='#67e8f9';ctx.lineWidth=2;ctx.strokeRect(mx+left/1440*mw,my+top/900*mh,(right-left)/1440*mw,(bottom-top)/900*mh);ctx.restore();
 }
};
await window.sceneReady;await window.renderAt(Number(params.get('t')||0));
if(!params.has('capture')){const started=performance.now();async function tick(){await window.renderAt(((performance.now()-started)/1000)%window.DURATION);requestAnimationFrame(tick);}requestAnimationFrame(tick);}
