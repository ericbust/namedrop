// env: FILM_SCALE (deviceScaleFactor, default 2), FILM_MAX_MB (the size the MP4 must fit, default 40),
// FILM_ONLY=poster (writes only the poster)
import { chromium } from 'playwright-core';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, rmSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { resolve } from 'node:path';

const [out, poster, base = 'http://127.0.0.1:8830'] = process.argv.slice(2);
if (!out || !poster) { console.error('usage: node drv/film.mjs <out.mp4> <poster.webp> [base-url]'); process.exit(2); }
const FPS = 30;
const SCALE = Number(process.env.FILM_SCALE || 2);
const MAX_BYTES = Number(process.env.FILM_MAX_MB || 40) * 1e6;
const POSTER = { ch: 'intro', beat: 'promise', at: 0.7 };
const DIR = resolve(import.meta.dirname, '..');
// /tmp is a small tmpfs; the lossless intermediate and Chromium's temp files go here and are removed.
const WORK = `${homedir()}/.cache/namedrop-film/run-${process.pid}`;
process.env.TMPDIR = WORK;
const LAUNCHES = [
  ['vulkan', ['--use-angle=vulkan', '--enable-features=Vulkan', '--enable-gpu', '--ignore-gpu-blocklist']],
  ['angle-gl', ['--use-angle=gl', '--enable-gpu', '--ignore-gpu-blocklist']],
  ['egl', ['--use-gl=egl', '--enable-gpu', '--ignore-gpu-blocklist']],
  ['swiftshader', ['--use-angle=swiftshader', '--enable-unsafe-swiftshader']],
];

const tl = await (await fetch(`${base}/timeline.json`)).json();
const starts = tl.chapters.map((_, i) => tl.chapters.slice(0, i).reduce((s, c) => s + c.duration, 0));
const total = tl.chapters.reduce((s, c) => s + c.duration, 0);
const frames = Math.round(total * FPS);
const frameAt = (n) => {
  const g = n / FPS;
  const ci = starts.findLastIndex((s) => s <= g);
  return [ci, g - starts[ci]];
};

const run = (args) => {
  const p = spawn('ffmpeg', ['-y', '-loglevel', 'error', ...args], { stdio: ['pipe', 'inherit', 'inherit'] });
  p.done = new Promise((ok, fail) => p.on('close', (code) => (code ? fail(new Error(`ffmpeg exited ${code}`)) : ok())));
  return p;
};
const probe = (file, stream) => Number(execFileSync('ffprobe', ['-v', 'error', '-select_streams', stream, '-show_entries', 'stream=duration', '-of', 'csv=p=0', file]).toString().trim());

async function launch() {
  for (const [name, args] of LAUNCHES) {
    const profile = `${WORK}/profile-${name}`;
    let ctx;
    try {
      ctx = await chromium.launchPersistentContext(profile, {
        executablePath: '/usr/bin/chromium', args, viewport: { width: 1920, height: 1080 }, deviceScaleFactor: SCALE, reducedMotion: 'no-preference',
      });
      const page = ctx.pages()[0] || await ctx.newPage();
      const gpu = await page.evaluate(() => {
        const gl = document.createElement('canvas').getContext('webgl2');
        const info = gl?.getExtension('WEBGL_debug_renderer_info');
        return gl ? gl.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : gl.RENDERER) : '';
      });
      if (gpu && (name === 'swiftshader' || !/swiftshader/i.test(gpu))) return { ctx, page, how: `${name}: ${gpu}` };
      console.error(`${name}: ${gpu || 'no WebGL2'}, trying the next`);
    } catch (err) {
      console.error(`${name}: ${err.message.split('\n')[0]}, trying the next`);
    }
    await ctx?.close();
    rmSync(profile, { recursive: true, force: true });
  }
  throw new Error('Chromium would not start with any WebGL backend');
}

async function open(page) {
  const log = [];
  page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') log.push(`${m.type()}: ${m.text()}`); });
  page.on('pageerror', (e) => log.push(`pageerror: ${e.message}`));
  await page.goto(`${base}/index.html?film`);
  await page.waitForFunction(() => document.body.dataset.ready === '1');
  await page.evaluate(() => document.fonts.ready);
  return log;
}

async function shootFrames(page) {
  const video = `${WORK}/video.mkv`;
  const enc = run(['-f', 'image2pipe', '-framerate', String(FPS), '-i', '-', '-vf', 'scale=1920:1080:flags=lanczos',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-qp', '0', video]);
  for (let n = 0; n < frames; n++) {
    await page.evaluate(([ci, t]) => window.renderAt(ci, t), frameAt(n));
    const png = await page.screenshot({ type: 'png' });
    if (!enc.stdin.write(png)) await new Promise((r) => enc.stdin.once('drain', r));
    if (n % 90 === 0) process.stderr.write(`\rframe ${n}/${frames}`);
  }
  enc.stdin.end();
  await enc.done;
  process.stderr.write(`\rframe ${frames}/${frames}\n`);
  return video;
}

async function shootPoster(page) {
  const ci = tl.chapters.findIndex((c) => c.id === POSTER.ch);
  const line = tl.chapters[ci].lines.find((l) => l.beat === POSTER.beat);
  await page.evaluate(([ci, t]) => window.renderAt(ci, t), [ci, line.start + POSTER.at * (line.end - line.start)]);
  await page.evaluate((label) => {
    const el = document.createElement('div');
    el.style.cssText = 'position:fixed;left:0;right:0;top:700px;bottom:0;z-index:20;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:22px;pointer-events:none';
    el.innerHTML = `<div style="width:176px;height:176px;border-radius:50%;display:grid;place-items:center;background:rgba(8,8,10,0.62);backdrop-filter:blur(8px);border:2px solid rgba(255,255,255,0.9);box-shadow:0 0 90px rgba(125,226,209,0.28),0 24px 70px rgba(0,0,0,0.7)">
      <svg viewBox="0 0 24 24" width="76" height="76" style="margin-left:12px"><path d="M6 3.5v17l15-8.5z" fill="#fff"/></svg></div>
      <div style="padding:8px 18px;border-radius:999px;background:rgba(8,8,10,0.7);border:1px solid rgba(255,255,255,0.18);font:600 22px/1 'Inter',sans-serif;color:#e5e7f0;letter-spacing:0.01em">${label}</div>`;
    document.body.appendChild(el);
  }, `Watch the overview · ${Math.floor(total / 60)}:${String(Math.round(total % 60)).padStart(2, '0')}`);
  const still = `${WORK}/poster.png`;
  await page.screenshot({ path: still, type: 'png' });
  await run(['-i', still, '-vf', 'scale=1600:-2:flags=lanczos', '-c:v', 'libwebp', '-quality', '90', poster]).done;
}

async function posterOnly() {
  const { ctx, page } = await launch();
  const log = await open(page);
  await shootPoster(page);
  await ctx.close();
  console.log(`${poster}: ${(statSync(poster).size / 1e3).toFixed(0)} kB, console ${log.length ? log.join(' | ') : 'clean'}`);
}

async function film() {
  const t0 = Date.now();
  const audio = `${WORK}/audio.wav`;
  const inputs = tl.chapters.flatMap((c) => ['-i', `${DIR}/${c.audio}`]);
  const trims = tl.chapters.map((c, i) => `[${i}:a]atrim=end=${c.duration},apad=whole_dur=${c.duration},asetpts=N/SR/TB[a${i}]`);
  const join = `${tl.chapters.map((_, i) => `[a${i}]`).join('')}concat=n=${tl.chapters.length}:v=0:a=1,aresample=48000,aformat=channel_layouts=stereo[a]`;
  await run([...inputs, '-filter_complex', [...trims, join].join(';'), '-map', '[a]', '-c:a', 'pcm_s16le', audio]).done;

  const { ctx, page, how } = await launch();
  console.log(`chromium ${how}, deviceScaleFactor ${SCALE}, ${frames} frames for ${total.toFixed(3)} s`);
  const log = await open(page);
  const video = await shootFrames(page);
  await shootPoster(page);
  await ctx.close();
  console.log(`console: ${log.length ? log.join(' | ') : 'clean'}`);
  const rendered = Date.now();

  let crf = 18;
  for (;;) {
    await run(['-i', video, '-i', audio, '-map', '0:v', '-map', '1:a', '-c:v', 'libx264', '-preset', 'slow', '-crf', String(crf),
      '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '160k', '-movflags', '+faststart', out]).done;
    if (statSync(out).size <= MAX_BYTES) break;
    console.log(`crf ${crf}: ${(statSync(out).size / 1e6).toFixed(1)} MB is over the limit`);
    crf += 2;
  }

  const v = probe(out, 'v:0'), a = probe(out, 'a:0');
  const ok = Math.abs(v - a) <= 1 / FPS && Math.abs(v - total) <= 1 / FPS;
  console.log(`${out}: crf ${crf}, ${(statSync(out).size / 1e6).toFixed(1)} MB, video ${v.toFixed(3)} s, audio ${a.toFixed(3)} s, timeline ${total.toFixed(3)} s, ${ok ? 'in sync' : 'OUT OF SYNC'}`);
  console.log(`${poster}: ${(statSync(poster).size / 1e3).toFixed(0)} kB`);
  console.log(`render ${((rendered - t0) / 1000).toFixed(0)} s, encode ${((Date.now() - rendered) / 1000).toFixed(0)} s`);
  if (!ok) process.exitCode = 1;
}

mkdirSync(WORK, { recursive: true });
try {
  await (process.env.FILM_ONLY === 'poster' ? posterOnly() : film());
} finally {
  rmSync(WORK, { recursive: true, force: true });
}
