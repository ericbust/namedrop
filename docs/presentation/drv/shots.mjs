import { chromium } from 'playwright-core';
import { mkdtempSync, rmSync, mkdirSync } from 'node:fs';

const out = process.argv[2];
const base = process.argv[3] || 'http://127.0.0.1:8830';
if (!out) { console.error('usage: node drv/shots.mjs <out-dir> [base-url]'); process.exit(2); }
mkdirSync(out, { recursive: true });
const tl = await (await fetch(`${base}/timeline.json`)).json();
const at60 = (l) => l.start + 0.6 * (l.end - l.start);

async function shoot(label, viewport, picks) {
  const dir = mkdtempSync('/tmp/claude-1000/pw-');
  const ctx = await chromium.launchPersistentContext(dir, { executablePath: '/usr/bin/chromium', args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'], viewport, deviceScaleFactor: 1 });
  const page = ctx.pages()[0] || await ctx.newPage();
  const log = [];
  page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') log.push(`${m.type()}: ${m.text()}`); });
  page.on('pageerror', (e) => log.push(`pageerror: ${e.message}`));
  await page.goto(`${base}/index.html`);
  await page.waitForFunction(() => document.querySelectorAll('#seek .seg').length > 0);
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${out}/${label}-start.png` });
  const [ci0, t0] = picks[0];
  await page.goto(`${base}/index.html?ch=${tl.chapters[ci0].id}&t=${t0.toFixed(2)}&still`);
  await page.waitForFunction(() => document.body.dataset.ready === '1');
  for (const [ci, t, name] of picks) {
    await page.evaluate(([ci, t]) => window.renderAt(ci, t), [ci, t]);
    await page.waitForTimeout(60);
    await page.screenshot({ path: `${out}/${label}-${name}.png` });
  }
  const scroll = await page.evaluate(() => [document.documentElement.scrollWidth, innerWidth]);
  console.log(`${label}: ${picks.length + 1} shots, scrollWidth ${scroll[0]} <= ${scroll[1]} ${scroll[0] <= scroll[1] ? 'ok' : 'OVERFLOW'}, console ${log.length ? log.join(' | ') : 'clean'}`);
  await ctx.close();
  rmSync(dir, { recursive: true, force: true });
}

const wide = tl.chapters.flatMap((c, ci) => c.lines.map((l, li) => [ci, at60(l), `${String(ci + 1).padStart(2, '0')}${String.fromCharCode(97 + li)}-${l.beat}`]));
const phone = tl.chapters.map((c, ci) => { const l = c.lines.find((l) => l.head) || c.lines[0]; return [ci, at60(l), `${String(ci + 1).padStart(2, '0')}-${l.beat}`]; });
await shoot('L', { width: 1920, height: 1080 }, wide);
await shoot('P', { width: 390, height: 844 }, phone);
