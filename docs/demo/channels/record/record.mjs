import { chromium } from 'playwright-core';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdir, readFile, writeFile, readdir, rename } from 'node:fs/promises';
import { promisify } from 'node:util';
import { execFile } from 'node:child_process';
import { performance } from 'node:perf_hooks';

const [profileName, directory] = process.argv.slice(2);
if (!profileName?.startsWith('test-') || !directory) throw new Error('Use record.mjs <test-profile> <output directory>');
const exec = promisify(execFile), root = resolve(directory);
const helper = resolve(dirname(fileURLToPath(import.meta.url)), 'profile.py');
const queue = root + '/actions';
await mkdir(queue, { recursive: true });
await mkdir(root + '/recording/frames', { recursive: true });
await mkdir(root + '/recording/panes', { recursive: true });

const VIEWPORT = { width: 1280, height: 720 }, SCALE = 2;
const profile = JSON.parse((await exec('python3', [helper, profileName, 'info'])).stdout);
if (!profile.productMatches) throw new Error('profile was not built from the expected product revision ' + profile.productRevision);
for (const alias of ['claude', 'codex']) {
  const address = profile.addresses.find(row => row.alias === alias);
  if (!address?.registered || !['idle', 'done'].includes(address.agent_status)) throw new Error(alias + ' must be registered and idle');
}
const opened = JSON.parse((await exec('python3', [helper, profileName, 'open'])).stdout);
// Chromium ignores --disable-spell-checking here, so the take's own browser profile turns
// spellcheck off; otherwise typed names such as @claude get a red underline.
const browserProfile = root + '/browser-profile';
await mkdir(browserProfile + '/Default', { recursive: true });
await writeFile(browserProfile + '/Default/Preferences', JSON.stringify({ browser: { enable_spellchecking: false }, spellcheck: { dictionaries: [], use_spelling_service: false } }));
const context = await chromium.launchPersistentContext(browserProfile, {
  ...(process.env.CHROME ? { executablePath: process.env.CHROME } : { channel: 'chrome' }),
  headless: process.env.HEADLESS !== '0',
  args: ['--no-sandbox'],
  viewport: VIEWPORT, deviceScaleFactor: SCALE, colorScheme: 'light',
});
const page = context.pages()[0] || await context.newPage();
page.setDefaultTimeout(15000);
const errors = [];
page.on('pageerror', e => errors.push(e.message));
await page.goto(opened.data.url);
await page.getByRole('combobox', { name: /^Message #/ }).waitFor();
console.log('READY');

const recording = {
  viewport: VIEWPORT, deviceScaleFactor: SCALE, version: profile.version,
  productRevision: profile.productRevision, sourceRevision: profile.source.commit,
  profileHome: profile.profileHome, events: [], frames: [], panes: [], errors,
};
let active = false, finished = false, start = 0, rate = 15;
const time = () => (performance.now() - start) / 1000;

// The focal element: a ring rectangle and the page's cap height for the smallest text a
// viewer must read inside it, in CSS pixels. plan.py finds the final ring and cap height in
// the frame itself and prints this page value beside them as a cross-check. Either
// argument may be a list of locators; the ring is then their union.
async function focus(ring, read = ring) {
  const boxes = await Promise.all([ring].flat().map(locator => locator.boundingBox()));
  const x = Math.min(...boxes.map(b => b.x)), y = Math.min(...boxes.map(b => b.y));
  const right = Math.max(...boxes.map(b => b.x + b.width)), bottom = Math.max(...boxes.map(b => b.y + b.height));
  const types = await Promise.all([read].flat().map(locator => locator.evaluate(element => {
    const canvas = document.createElement('canvas').getContext('2d');
    const measure = styled => {
      const style = getComputedStyle(styled);
      canvas.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
      return { fontPx: parseFloat(style.fontSize), capPx: canvas.measureText('H').actualBoundingBoxAscent };
    };
    const rows = [];
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, { acceptNode: n => n.textContent.trim() ? 1 : 3 });
    for (let node = walker.nextNode(); node; node = walker.nextNode()) rows.push(measure(node.parentElement));
    if (!rows.length && element.value) rows.push(measure(element));
    const smallest = rows.sort((a, b) => a.capPx - b.capPx)[0];
    return { text: (element.value || element.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 240), ...smallest };
  })));
  const smallest = types.sort((a, b) => a.capPx - b.capPx)[0];
  return { ring: { x, y, width: right - x, height: bottom - y }, read: { ...smallest, text: types.map(t => t.text).join(' / ') } };
}
async function mark(name, ring, read) {
  const event = { name, t: time(), at: new Date().toISOString(), ...(ring ? { focus: await focus(ring, read) } : {}) };
  recording.events.push(event);
  console.log('EVENT', name, event.t.toFixed(2), event.focus?.read?.text ?? '');
  return event;
}
async function save() {
  recording.duration = time();
  await writeFile(root + '/recording/recording.json.tmp', JSON.stringify(recording, null, 1));
  await rename(root + '/recording/recording.json.tmp', root + '/recording/recording.json');
}
const record = {
  start() {
    if (active) throw new Error('already recording');
    recording.startedAtEpochMs = Date.now();
    start = performance.now();
    active = true;
    console.log('RECORDING');
  },
  rate(value) { rate = value; },
  stop() { finished = true; },
  helper: async (...args) => (await exec('python3', [helper, profileName, ...args])).stdout,
};

const frameJob = (async () => {
  while (!finished) {
    if (!active) { await new Promise(r => setTimeout(r, 100)); continue; }
    const t = time(), file = 'frames/' + String(recording.frames.length).padStart(6, '0') + '.jpg';
    await page.screenshot({ path: root + '/recording/' + file, type: 'jpeg', quality: 92 });
    recording.frames.push({ t, file });
    if (recording.frames.length % 30 === 0) await save();
    await new Promise(r => setTimeout(r, Math.max(0, 1000 / rate - (time() - t) * 1000)));
  }
})();

// Both agents' real screens, with colour, twice a second.
const paneJob = (async () => {
  while (!finished) {
    if (!active) { await new Promise(r => setTimeout(r, 100)); continue; }
    const t = time(), index = String(recording.panes.length).padStart(6, '0'), row = { t };
    await Promise.all(['claude', 'codex'].map(async alias => {
      const file = 'panes/' + index + '-' + alias + '.ansi';
      await writeFile(root + '/recording/' + file, await record.helper('ansi', alias));
      row[alias] = file;
    }));
    recording.panes.push(row);
    await new Promise(r => setTimeout(r, Math.max(0, 500 - (time() - t) * 1000)));
  }
})();

const seen = new Set();
try {
  while (!finished) {
    for (const file of (await readdir(queue)).filter(x => x.endsWith('.json')).sort()) {
      if (seen.has(file)) continue;
      seen.add(file);
      try {
        const run = new Function('return (' + JSON.parse(await readFile(queue + '/' + file, 'utf8')).code + ')')();
        const value = await run({ page, mark, focus, record, out: root });
        await writeFile(queue + '/' + file + '.result', JSON.stringify({ ok: true, value }, null, 2));
      } catch (e) {
        await page.screenshot({ path: root + '/action-failure.png' });
        await writeFile(queue + '/' + file + '.result', JSON.stringify({ ok: false, error: e.stack }));
        console.log('ACTION FAILED', file, e.message);
      }
    }
    await new Promise(r => setTimeout(r, 150));
  }
} finally {
  finished = true;
  await Promise.all([frameJob, paneJob]);
  if (start) await save();
  await context.close();
  console.log('SAVED', recording.frames.length, 'frames,', recording.panes.length, 'pane pairs');
}
