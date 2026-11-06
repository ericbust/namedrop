import * as THREE from 'three';
import {CSS3DRenderer,CSS3DObject} from 'three/addons/renderers/CSS3DRenderer.js';

const W=1920,H=1080;
const scene=new THREE.Scene(),cssScene=new THREE.Scene();
const camera=new THREE.PerspectiveCamera(35,W/H,1,10000);
camera.position.z=(H/2)/Math.tan(THREE.MathUtils.degToRad(17.5));
const gl=new THREE.WebGLRenderer({canvas:document.querySelector('#gl'),antialias:true,alpha:true});
gl.setSize(W,H);
const css=new CSS3DRenderer();css.setSize(W,H);document.querySelector('#css').append(css.domElement);
const caption=document.querySelector('#caption'),end=document.querySelector('#end');
const shots=[
 {start:0,end:6,title:'One page for your agents.',sub:'Claude Code and Codex, together in Herdr.',image:'captures/01-overview.png',width:1440,height:900,layout:'overview'},
 {start:6,end:12,title:'Ask in a channel.',sub:'Mention @codex. The reply lands in the thread.',image:'crops/channel.png',width:1150,height:180,layout:'channel',card:'Confirm scope, pass release tests, and prepare the rollback plan.'},
 {start:12,end:18,title:'Follow up without @.',sub:'One agent in the thread. Namedrop knows who you mean.',image:'crops/thread.png',width:385,height:255,layout:'thread',detail:'crops/followup.png',detailWidth:385,detailHeight:85},
 {start:18,end:24,title:'A direct message. A reminder.',sub:'Claude confirms now. Cancel while it is pending.',image:'crops/dm-request.png',width:1120,height:140,layout:'dm',detail:'crops/dm-pending.png',detailWidth:365,detailHeight:128},
 {start:24,end:29,title:'2 minutes later.',sub:'The reminder and its answer return to the same DM.',image:'crops/dm-fired.png',width:850,height:110,layout:'fired'},
 {start:29,end:35,title:'Look in. Join in.',sub:'Live Peek fills the frame. Type straight to your agent.',image:'crops/peek-reply.png',width:380,height:230,layout:'peek',detail:'crops/peek-input.png',detailWidth:380,detailHeight:150},
];
const objects=[];
function object(html,x,y,z=0){
 const el=document.createElement('div');el.innerHTML=html;
 const obj=new CSS3DObject(el);obj.position.set(x,y,z);cssScene.add(obj);objects.push(obj);return obj;
}
const screen=object('<div class="screen"><div class="bar">Namedrop · Channels (Beta)</div><img></div>',0,-80);
const detail=object('<div class="screen"><div class="bar"></div><img></div>',0,-80);
const plates=[object('<span class="plate codex">● @codex <em>Codex</em></span>',620,260),object('<span class="plate claude">● @claude <em>Claude Code</em></span>',620,-130)];
const panes=['codex','claude'].map((alias,i)=>{
 const pane=object('<div class="screen"><div class="bar"></div><div class="terminal"></div></div>',620,110-i*390);
 pane.element.querySelector('.bar').textContent=alias==='codex'?'Codex':'Claude Code';
 return pane;
});
const card=object('<div class="card"><small>@codex → You</small><div></div></div>',0,-300,100);
const beam=new THREE.Mesh(new THREE.PlaneGeometry(500,3),new THREE.MeshBasicMaterial({color:'#67e8f9',transparent:true,opacity:0.7}));
beam.position.set(0,-65,80);scene.add(beam);
let working='',terminals={};
plates.forEach(o=>{o.position.y+=24;o.position.z=20;});
const clamp=x=>Math.max(0,Math.min(1,x));
const ready=(async()=>{
 terminals=await (await fetch('terminals.json')).json();working=terminals.working;
 panes.forEach((p,i)=>{const text=p.element.querySelector('.terminal');text.textContent=terminals[['codex','claude'][i]];text.style.width='570px';text.style.height='245px';});
 const sources=[...new Set([...shots.flatMap(s=>[s.image,...(s.detail?[s.detail]:[])]),'crops/followup-result.png','crops/ask.png'])];
 await Promise.all(sources.map(src=>new Promise((resolve,reject)=>{const image=new Image();image.onload=resolve;image.onerror=()=>reject(new Error(src));image.src=src;})));
})();
export const DURATION=40;
window.DURATION=DURATION;
function picture(obj,src,width,height,targetWidth,x,y){
 const image=obj.element.querySelector('img');image.src=src;image.width=width;image.height=height;
 obj.scale.setScalar(targetWidth/width);obj.position.set(x,y,0);obj.rotation.y=0;
}
window.renderAt=t=>{
 const shot=shots.find(s=>t>=s.start&&t<s.end),final=t>=35;
 end.style.opacity=final?clamp((t-35)/.5):0;end.style.display=final?'flex':'none';caption.style.opacity=final?0:1;
 objects.forEach(o=>o.visible=false);beam.visible=false;
 if(shot){
  const local=t-shot.start,overview=shot.layout==='overview';
  caption.querySelector('h1').textContent=shot.title;document.querySelector('#subtitle').textContent=shot.sub;caption.style.opacity=clamp(local/.35);
  screen.visible=true;
  picture(screen,shot.image,shot.width,shot.height,1500,0,-40);
  if(overview){
   picture(screen,shot.image,shot.width,shot.height,1070,-320,-70);screen.rotation.y=.08;
   plates.forEach(o=>{o.visible=true;o.scale.setScalar(.9);});panes.forEach((o,i)=>{o.element.querySelector('.terminal').textContent=terminals[['codex','claude'][i]];o.visible=true;o.position.set(620,110-i*390,0);o.scale.setScalar(.9);});
  }
  if(shot.layout==='channel'){
   screen.position.y=90;card.visible=local>2.4;
   if(local<2){picture(screen,'crops/ask.png',1150,85,1500,0,90);panes[0].visible=true;panes[0].position.set(560,-230,0);panes[0].scale.setScalar(.9);panes[0].element.querySelector('.terminal').textContent=working;}
   if(card.visible){card.element.querySelector('.card div').textContent=shot.card;card.element.style.opacity=clamp((local-2.4)/.4);}
   beam.visible=local<2;beam.position.set(270,-80,50);beam.rotation.z=-.35;beam.scale.x=clamp(local/1.4)*1.5;
  }
  detail.visible=!!shot.detail;
  if(shot.layout==='thread'){
   picture(screen,shot.image,shot.width,shot.height,770,-460,-10);
   picture(detail,local>3?'crops/followup-result.png':shot.detail,385,local>3?155:85,770,440,-90);detail.element.querySelector('.bar').textContent=local>3?'The reply lands in the thread':'A plain follow-up · To @codex';
  }
  if(shot.layout==='dm'){
   screen.position.y=120;picture(detail,shot.detail,shot.detailWidth,shot.detailHeight,850,0,-230);detail.element.querySelector('.bar').textContent='Pending reminder · Cancel anytime';
  }
  if(shot.layout==='peek'){
   picture(screen,shot.image,shot.width,shot.height,860,-440,-70);screen.element.querySelector('.bar').textContent='@codex · Live Peek';
   picture(detail,shot.detail,shot.detailWidth,shot.detailHeight,730,450,-70);detail.element.querySelector('.bar').textContent='Type to agent';
  }else screen.element.querySelector('.bar').textContent='Namedrop · Channels (Beta)';
 }
 gl.render(scene,camera);css.render(cssScene,camera);
};
window.sceneReady=ready;
await ready;
window.renderAt(Number(new URLSearchParams(location.search).get('t')||0));
if(!new URLSearchParams(location.search).has('capture')){
 const started=performance.now();function tick(now){window.renderAt(((now-started)/1000)%DURATION);requestAnimationFrame(tick);}requestAnimationFrame(tick);
}
