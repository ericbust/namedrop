// node stills.mjs — README still and social preview, rendered at 2x
import { chromium } from 'playwright-core';
const url = (process.env.SCENE_URL || 'http://127.0.0.1:8765/scene.html') + '?capture&still';
const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu'] });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 2 });
await page.goto(url);
await page.evaluate(() => window.sceneReady);
// the scene says when: Agent status once it has settled with its spotlight, and the end card
const { status, social } = await page.evaluate(() => window.STILLS);
await page.evaluate((t) => window.renderAt(t), status);
const box = await page.evaluate(() => {
  const r = [...document.querySelectorAll('.win')].at(-1).getBoundingClientRect();
  return { x: r.x, y: r.y, width: r.width, height: r.height };
});
await page.screenshot({ path: 'agent-status.png', clip: { x: box.x - 24, y: box.y - 24, width: box.width + 48, height: box.height + 48 } });
// social preview: end card, 2:1
await page.evaluate((t) => window.renderAt(t), social);
await page.screenshot({ path: 'social-2x.png', clip: { x: 0, y: 60, width: 1920, height: 960 } });
await browser.close();
