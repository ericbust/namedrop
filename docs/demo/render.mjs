// node render.mjs out.mp4 [fps] — step the scene frame by frame into ffmpeg, at 2x (3840×2160) for crisp text
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
const [out = 'namedrop-demo.mp4', fps = '30'] = process.argv.slice(2);
const url = (process.env.SCENE_URL || 'http://127.0.0.1:8765/scene.html') + '?capture&still';
const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu'] });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 2 });
page.on('pageerror', (e) => { console.error(e.stack); process.exit(1); });
await page.goto(url);
await page.evaluate(() => window.sceneReady);
const duration = await page.evaluate(() => window.DURATION);
const ffmpeg = spawn('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'image2pipe', '-framerate', fps, '-i', '-',
  '-c:v', 'libx264', '-preset', 'slow', '-crf', '16', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', out],
  { stdio: ['pipe', 'inherit', 'inherit'] });
const frames = Math.round(duration * Number(fps));
for (let i = 0; i < frames; i++) {
  await page.evaluate((t) => window.renderAt(t), i / Number(fps));
  const png = await page.screenshot({ type: 'png' });
  if (!ffmpeg.stdin.write(png)) await new Promise((r) => ffmpeg.stdin.once('drain', r));
  if (i % 60 === 0) process.stderr.write(`\r${i}/${frames}`);
}
ffmpeg.stdin.end();
await new Promise((r) => ffmpeg.on('close', r));
await browser.close();
process.stderr.write(`\rwrote ${out} (${frames} frames)\n`);
