// node render.mjs <take-dir> <out.mp4>                    render the movie at 2560×1440, 30 fps
// node render.mjs <take-dir> --stills <t,...> <out,...>     render single frames as PNG
// Serves this directory at / and the take at /take/, and steps scene.html frame by frame into ffmpeg.
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const [takeArg, ...rest] = process.argv.slice(2);
if (!takeArg || !rest.length) {
  console.error('usage: node render.mjs <take-dir> <out.mp4> | --stills <t,...> <out,...>');
  process.exit(2);
}
const take = path.resolve(takeArg);
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.json': 'application/json', '.jpg': 'image/jpeg', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const url = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const [root, rel] = url.startsWith('/take/') ? [take, url.slice(6)] : [here, url.slice(1)];
  const file = path.join(root, rel);
  if (!file.startsWith(root) || !fs.existsSync(file) || !fs.statSync(file).isFile()) { res.writeHead(404); return res.end(); }
  res.writeHead(200, { 'content-type': TYPES[path.extname(file)] || 'application/octet-stream' });
  fs.createReadStream(file).pipe(res);
}).listen(0, '127.0.0.1');
await new Promise((resolve) => server.once('listening', resolve));

const browser = await chromium.launch({
  ...(process.env.CHROME ? { executablePath: process.env.CHROME } : { channel: 'chrome' }),
  args: process.platform === 'darwin' ? ['--use-angle=metal', '--enable-gpu'] : ['--no-sandbox'],
});
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 4 / 3 });
page.on('pageerror', (e) => { console.error(e.stack); process.exit(1); });
await page.goto(`http://127.0.0.1:${server.address().port}/scene.html`);
await page.waitForFunction(() => window.sceneReady);
await page.evaluate(() => window.sceneReady);

if (rest[0] === '--stills') {
  const times = rest[1].split(',').map(Number), outs = rest[2].split(',');
  for (const [i, t] of times.entries()) {
    await page.evaluate((t) => window.renderAt(t), t);
    await page.screenshot({ path: outs[i] });
  }
} else {
  const fps = 30, out = path.resolve(rest[0]);
  const duration = await page.evaluate(() => window.DURATION);
  const ffmpeg = spawn('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'image2pipe', '-framerate', String(fps), '-i', '-',
    '-c:v', 'libx264', '-preset', 'slow', '-crf', '12', '-pix_fmt', 'yuv420p', out], { stdio: ['pipe', 'inherit', 'inherit'] });
  const frames = Math.round(duration * fps);
  for (let i = 0; i < frames; i++) {
    await page.evaluate((t) => window.renderAt(t), i / fps);
    const png = await page.screenshot({ type: 'png' });
    if (!ffmpeg.stdin.write(png)) await new Promise((resolve) => ffmpeg.stdin.once('drain', resolve));
    if (i % 60 === 0) process.stderr.write(`\r${i}/${frames}`);
  }
  ffmpeg.stdin.end();
  await new Promise((resolve, reject) => ffmpeg.on('close', (code) => (code === 0 ? resolve() : reject(new Error(`ffmpeg exited ${code}`)))));
  process.stderr.write(`\rwrote ${out} (${frames} frames)\n`);
}
await browser.close();
server.close();
