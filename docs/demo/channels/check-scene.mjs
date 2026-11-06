import assert from 'node:assert/strict';
import {chromium} from 'playwright-core';
import {mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const output=resolve(process.argv[2]||'channels-preview');await mkdir(output,{recursive:true});
const b=await chromium.launch({...(process.env.CHROME?{executablePath:process.env.CHROME}:{channel:'chrome'}),args:['--no-sandbox']});
const p=await b.newPage({viewport:{width:1920,height:1080}});
const errors=[];p.on('pageerror',e=>errors.push(e.message));
await p.goto((process.env.SCENE_URL||'http://127.0.0.1:8767/channels/scene.html')+'?capture&still');await p.waitForFunction(()=>window.sceneReady&&window.renderAt);await p.evaluate(()=>window.sceneReady);
await p.evaluate(()=>window.renderAt(3));const a=await p.screenshot();
for(const t of [7,9,14,16,21,26,32,37]){await p.evaluate(t=>window.renderAt(t),t);await p.screenshot({path:join(output,`scene-${t}.png`)});}
await p.evaluate(()=>window.renderAt(3));const c=await p.screenshot();
assert.ok(a.equals(c),'seeking back to the overview reproduces the same frame');
await p.screenshot({path:join(output,'scene-3.png')});
assert.deepEqual(errors,[]);console.log('PASS. Every beat renders without page errors, and seeking reproduces the overview.');await b.close();
