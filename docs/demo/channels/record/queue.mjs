import {readFile,writeFile,rename,mkdir} from 'node:fs/promises';
import {resolve,basename,join} from 'node:path';
const [directory,script]=process.argv.slice(2);
if(!directory||!script)throw new Error('Use queue.mjs <capture directory> <trusted action.js>');
const queue=resolve(directory,'actions');await mkdir(queue,{recursive:true});
const target=join(queue,basename(script).replace(/\.js$/,'.json'));
await writeFile(target+'.tmp',JSON.stringify({code:await readFile(script,'utf8')}));
await rename(target+'.tmp',target);
