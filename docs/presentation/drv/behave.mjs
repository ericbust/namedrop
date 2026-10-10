import { chromium } from 'playwright-core';
import { mkdtempSync, rmSync } from 'node:fs';
const BASE = process.argv[2] || 'http://127.0.0.1:8830';
const URL0 = `${BASE}/index.html`;
const tl = await (await fetch(`${BASE}/timeline.json`)).json();
const fileOf = (i) => new RegExp(`^${Math.round(tl.chapters[i].duration)}s-file@[1-9]`);
const clock = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
const ch3Start = tl.chapters[0].duration + tl.chapters[1].duration;
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (ok, what) => { console.log(`${ok ? 'PASS' : 'FAIL'} ${what}`); if (!ok) failures++; };

async function open(url, route) {
  const dir = mkdtempSync('/tmp/claude-1000/pw-');
  const ctx = await chromium.launchPersistentContext(dir, { executablePath: '/usr/bin/chromium', args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'], viewport: { width: 1280, height: 720 } });
  const page = ctx.pages()[0] || await ctx.newPage();
  const log = [];
  page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') log.push(`${m.type()}: ${m.text()}`); });
  page.on('pageerror', (e) => log.push(`pageerror: ${e.message}`));
  await page.addInitScript(() => {
    const play = HTMLMediaElement.prototype.play;
    window.__media = new Set();
    HTMLMediaElement.prototype.play = function () { window.__media.add(this); return play.call(this); };
  });
  if (route) await route(page);
  await page.goto(url);
  await page.waitForFunction(() => document.querySelectorAll('#seek .seg').length === 8);
  const close = async () => { await ctx.close(); rmSync(dir, { recursive: true, force: true }); };
  return { page, log, close };
}
const state = (page) => page.evaluate(() => {
  const playing = [...window.__media].find((a) => !a.paused);
  return {
    ch: document.getElementById('kNum').textContent.slice(0, 2),
    now: document.getElementById('tNow').textContent,
    wall: getComputedStyle(document.getElementById('audioNote')).display !== 'none',
    audio: playing ? `${Math.round(playing.duration)}s-file@${playing.currentTime.toFixed(1)}` : 'none',
    idle: document.body.classList.contains('idle'),
    bar: getComputedStyle(document.getElementById('bar')).opacity,
    captions: getComputedStyle(document.getElementById('caption')).visibility,
    hero: document.querySelector('#stage .hero')?.textContent.trim() || '',
  };
});
const fmt = (s) => `ch${s.ch} ${s.now} wall=${s.wall} audio=${s.audio}`;

{
  const { page, log, close } = await open(`${URL0}?ch=intro&t=5`, (p) => p.route('**/voice/intro.mp3', (r) => r.abort()));
  await page.click('#startPlay');
  await wait(1500);
  const a = await state(page);
  await wait(1500);
  const b = await state(page);
  check(a.ch === '02' && a.wall && a.audio === 'none' && a.now !== b.now, `blocked intro.mp3: chapter 2 runs on the wall clock (${fmt(a)} -> ${fmt(b)})`);
  await page.waitForFunction(() => document.getElementById('kNum').textContent.startsWith('03'), null, { timeout: 12000 });
  await wait(2500);
  const c = await state(page);
  check(c.ch === '03' && !c.wall && fileOf(2).test(c.audio), `the next chapter plays on its own audio (${fmt(c)})`);
  await page.keyboard.press('ArrowLeft');
  await wait(400);
  const d = await state(page);
  check(d.ch === '03' && [clock(ch3Start), clock(ch3Start + 1)].includes(d.now), `Left arrow more than 2 s in restarts the chapter (${fmt(d)})`);
  await page.keyboard.press('ArrowLeft');
  await wait(1200);
  const e = await state(page);
  check(e.ch === '02' && e.wall && e.audio === 'none', `Left arrow within 2 s goes back; the known-bad chapter stays on the wall clock (${fmt(e)})`);
  await page.keyboard.press('ArrowRight');
  await wait(2500);
  const f = await state(page);
  check(f.ch === '03' && !f.wall && fileOf(2).test(f.audio), `Right arrow: chapter 3 is back on audio (${fmt(f)})`);
  console.log('console:', log.length ? log.join(' | ') : 'clean');
  check(log.every((l) => /intro\.mp3|intro"|ERR_FAILED|Failed to load resource/.test(l)), 'only the blocked chapter logged anything');
  await close();
}

{
  const { page, log, close } = await open(`${URL0}?ch=mention&t=15`);
  const s0 = await state(page);
  check(s0.captions === 'hidden', `captions are off by default (visibility ${s0.captions})`);
  await page.click('#startPlay');
  await wait(2500);
  const a = await state(page);
  const t = parseFloat(a.audio.split('@')[1]);
  check(a.ch === '03' && t >= 15 && t < 19, `starting mid-chapter waits for the seek target instead of snapping to 0 (${fmt(a)})`);
  check(a.hero === 'The answer comes back.' && a.captions === 'hidden', `the line's head is the hero type and the caption stays hidden (hero "${a.hero}")`);
  await page.keyboard.press('c');
  await wait(100);
  const cc = await state(page);
  check(cc.captions === 'visible', `C turns captions on (visibility ${cc.captions})`);
  await page.keyboard.press('c');
  await page.mouse.move(640, 360);
  await wait(3200);
  const idle = await state(page);
  check(idle.idle && idle.bar === '0', `controls hide after 2.5 s without pointer movement (idle=${idle.idle}, bar opacity ${idle.bar})`);
  await page.mouse.move(660, 380);
  await wait(700);
  const back = await state(page);
  check(!back.idle && back.bar === '1', `controls return on pointer movement (idle=${back.idle}, bar opacity ${back.bar})`);
  const box = await page.locator('#seek').boundingBox();
  await page.mouse.click(box.x + box.width * 0.6, box.y + box.height / 2);
  await wait(400);
  const b0 = await state(page);
  await wait(2500);
  const b = await state(page);
  const [m0, s0b] = b0.now.split(':').map(Number), [m1, s1] = b.now.split(':').map(Number);
  check(b.ch === '06' && !b.wall && b.audio !== 'none' && parseFloat(b.audio.split('@')[1]) > 3 && (m1 * 60 + s1) - (m0 * 60 + s0b) >= 2, `a seek-bar seek mid-chapter plays on from the target (${fmt(b0)} -> ${fmt(b)})`);
  console.log('console:', log.length ? log.join(' | ') : 'clean');
  check(log.length === 0, 'console is clean on a normal run');
  await close();
}

{
  const { page, log, close } = await open(URL0, (p) => p.route('**/index.html', async (r) => {
    const res = await r.fetch();
    await r.fulfill({ response: res, headers: { ...res.headers(), 'content-security-policy': "media-src 'self'" } });
  }));
  await page.click('#startPlay');
  await wait(3000);
  const a = await state(page);
  check(a.ch === '01' && !a.wall && fileOf(0).test(a.audio), `with blob: refused by CSP, the plain URL plays (${fmt(a)})`);
  console.log('console:', log.length ? log.join(' | ') : 'clean');
  await close();
}

console.log(failures ? `${failures} FAILED` : 'ALL PASS');
process.exit(failures ? 1 : 0);
