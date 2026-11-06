import {chromium} from 'playwright-core';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {mkdir,readFile,writeFile,readdir,rename} from 'node:fs/promises';
import {promisify} from 'node:util';
import {execFile} from 'node:child_process';
import {performance} from 'node:perf_hooks';
const [profileName,directory]=process.argv.slice(2);
if(!profileName?.startsWith('test-')||!directory)throw new Error('Use record.mjs <test-profile> <output directory>');
const exec=promisify(execFile),root=resolve(directory),out=root;
const helper=resolve(dirname(fileURLToPath(import.meta.url)),'profile.py');
const queue=root+'/actions';await mkdir(queue,{recursive:true});await mkdir(root+'/recording/frames',{recursive:true});await mkdir(root+'/recording/panes',{recursive:true});
const profile=JSON.parse((await exec('python3',[helper,profileName,'info'])).stdout);
if(profile.version!=='1.5.0-exp.17'||!profile.productMatches)throw new Error('Final capture requires the verified exp.17 product');
for(const alias of ['claude','codex']){
 const address=profile.addresses.find(row=>row.alias===alias);
 if(!address?.registered||!['idle','done'].includes(address.agent_status))throw new Error(alias+' must be registered and idle');
}
const opened=JSON.parse((await exec('python3',[helper,profileName,'open'])).stdout);
const browser=await chromium.launch({...(process.env.CHROME?{executablePath:process.env.CHROME}:{channel:'chrome'}),headless:process.env.HEADLESS!=='0',args:['--no-sandbox','--disable-spell-checking','--disable-features=Spellcheck,SpellcheckService']});
const context=await browser.newContext({viewport:{width:1440,height:900},deviceScaleFactor:2,colorScheme:'light'});
const page=await context.newPage();page.setDefaultTimeout(15000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
await page.goto(opened.data.url);await page.getByRole('combobox',{name:'Message #release-planning',exact:true}).waitFor();console.log('READY');
const recording={viewport:{width:1440,height:900},deviceScaleFactor:2,sourceRevision:profile.source.commit,productRevision:'8ea5c1a21d61da0e2539a2d8cae0638557c83dab',version:profile.version,throwaway:false,profileHome:profile.profileHome,profileRoot:profile.profileRoot,events:[],frames:[],panes:[],errors};
let active=false,finished=false,start=0,rate=12;
const time=()=>((performance.now()-start)/1000);
async function mark(name,rect){const e={name,t:time(),...(rect?{rect}:{}),at:new Date().toISOString()};recording.events.push(e);console.log('EVENT',name,e.t.toFixed(2));return e;}
async function save(){recording.duration=time();await writeFile(root+'/recording/recording.json.tmp',JSON.stringify(recording,null,2));await rename(root+'/recording/recording.json.tmp',root+'/recording/recording.json');}
const record={async cancel(alias){await exec('python3',[helper,profileName,'cancel',alias]);},async start(){if(active)throw new Error('already recording');recording.startedAtEpochMs=Date.now();start=performance.now();active=true;console.log('RECORDING');},rate(value){rate=value;},async stop(){active=false;finished=true;}};
const frameJob=(async()=>{while(!finished){if(!active){await new Promise(r=>setTimeout(r,100));continue;}const t=time(),file='frames/'+String(recording.frames.length).padStart(6,'0')+'.jpg';await page.screenshot({path:root+'/recording/'+file,type:'jpeg',quality:94});recording.frames.push({t,file});await new Promise(r=>setTimeout(r,Math.max(0,1000/rate-(time()-t)*1000)));} })();
const paneJob=(async()=>{while(!finished){if(!active){await new Promise(r=>setTimeout(r,100));continue;}const t=time(),idx=String(recording.panes.length).padStart(6,'0');const row={t};await Promise.all(['claude','codex'].map(async alias=>{const {stdout}=await exec('python3',[helper,profileName,'pane',alias],{maxBuffer:1024*1024});const file='panes/'+idx+'-'+alias+'.ansi';await writeFile(root+'/recording/'+file,stdout);row[alias]=file;}));recording.panes.push(row);await save();await new Promise(r=>setTimeout(r,500));}})();
const seen=new Set();
try{while(!finished){for(const file of (await readdir(queue)).filter(x=>x.endsWith('.json')).sort()){if(seen.has(file))continue;seen.add(file);let action;try{action=JSON.parse(await readFile(queue+'/'+file,'utf8'));}catch(e){await writeFile(queue+'/'+file+'.result',JSON.stringify({ok:false,error:e.message}));continue;}try{const run=new Function('return ('+action.code+')')();const value=await run({page,context,mark,out,record,profile});await writeFile(queue+'/'+file+'.result',JSON.stringify({ok:true,value},null,2));}catch(e){await page.screenshot({path:out+'/action-failure.png'});await writeFile(queue+'/'+file+'.result',JSON.stringify({ok:false,error:e.stack}));console.log('ACTION FAILED',file,e.message);}}await new Promise(r=>setTimeout(r,150));}
}finally{finished=true;await Promise.allSettled([frameJob,paneJob]);if(start)await save();await browser.close();console.log('SAVED',recording.frames.length,'browser frames',recording.panes.length,'pane pairs');}
