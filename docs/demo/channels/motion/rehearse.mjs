import {chromium} from 'playwright-core';
import {resolve} from 'node:path';
import {mkdir,writeFile} from 'node:fs/promises';
import {performance} from 'node:perf_hooks';
const [url,directory,revision]=process.argv.slice(2);
if(!url||!directory||!revision||!process.argv.includes('--rehearsal')||new URL(url).hostname!=='127.0.0.1')throw new Error('Use record.mjs <testbed loopback URL> <output directory> <source revision> --rehearsal');
const output=resolve(directory);await mkdir(output+'/frames',{recursive:true});
const browser=await chromium.launch({...(process.env.CHROME?{executablePath:process.env.CHROME}:{channel:'chrome'}),args:['--no-sandbox']});
const context=await browser.newContext({viewport:{width:1440,height:900},deviceScaleFactor:2,colorScheme:'light'});
const page=await context.newPage();page.setDefaultTimeout(60000);
await page.goto(url);
await page.getByRole('combobox',{name:'Message #release-rehearsal',exact:true}).waitFor();
const startedAtEpochMs=Date.now(),start=performance.now(),frames=[],events=[];let running=true;
const mark=(name)=>{const t=(performance.now()-start)/1000;events.push({name,t});console.log(name,t.toFixed(2));};
const recorder=(async()=>{while(running){const t=(performance.now()-start)/1000;const file='frames/'+String(frames.length).padStart(6,'0')+'.jpg';await page.screenshot({path:output+'/'+file,type:'jpeg',quality:94});frames.push({t,file});await new Promise(r=>setTimeout(r,70));}})();
try{
 mark('wide');await page.waitForTimeout(2500);
 const composer=page.getByRole('combobox',{name:'Message #release-rehearsal',exact:true});
 mark('type');await composer.pressSequentially('@claude Give three release checks in one short sentence.',{delay:95});await composer.press('Escape');mark('instruction');await page.waitForTimeout(1800);
 await composer.press('Enter');mark('posted');
 await page.locator('.reply-preview').first().waitFor({timeout:120000});mark('reply');await page.waitForTimeout(1600);await page.locator('.reply-preview').first().click();mark('thread');await page.waitForTimeout(1800);
 const thread=page.getByRole('complementary',{name:'Thread',exact:true});mark('followup-type');await thread.getByRole('combobox').pressSequentially('Make it five words.',{delay:100});mark('followup');await page.waitForTimeout(2200);await thread.getByRole('combobox').press('Enter');mark('followup-posted');await page.waitForTimeout(5000);mark('end');
}finally{
 running=false;await recorder;await writeFile(output+'/recording.json',JSON.stringify({viewport:{width:1440,height:900},deviceScaleFactor:2,sourceRevision:revision,startedAtEpochMs,throwaway:true,duration:(performance.now()-start)/1000,events,frames},null,2));await browser.close();console.log('Saved',frames.length,'real frames');
}
