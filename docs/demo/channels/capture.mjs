import {chromium} from 'playwright-core';
import {mkdir} from 'node:fs/promises';
import {createInterface} from 'node:readline/promises';
import {resolve,join} from 'node:path';

const [url,directory]=process.argv.slice(2);
if(!url||!directory||new URL(url).hostname!=='127.0.0.1')throw new Error('Use capture.mjs <testbed loopback URL> <output directory>');
const output=resolve(directory);await mkdir(output,{recursive:true});
const browser=await chromium.launch({...(process.env.CHROME?{executablePath:process.env.CHROME}:{channel:'chrome'}),headless:false});
const context=await browser.newContext({viewport:{width:1440,height:900},deviceScaleFactor:2,colorScheme:'light'});
const page=await context.newPage();await page.goto(url);
const input=createInterface({input:process.stdin,output:process.stdout});
try{
 while(true){
  const name=(await input.question('Capture name, or quit: ')).trim();
  if(name==='quit')break;
  if(!/^[a-zA-Z0-9_-]+$/.test(name)){console.log('Use letters, digits, underscores or hyphens.');continue;}
  await page.screenshot({path:join(output,name+'.png')});
  console.log('Saved '+name+'.png');
 }
}finally{input.close();await browser.close();}
