import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium } from "playwright-core";
import { open, full, clickMember, until } from "./live-peek.browser.mjs";

const fixture = JSON.parse(await readFile(new URL("./fixtures/blank-736-737.json", import.meta.url), "utf8"));
const encode = (value) => Buffer.from(value).toString("base64");
const seed = fixture.seed.text.split("\n").map((row, i) => `\x1b[${i + 1};1H${row}`).join("");
const native = (seq) => fixture.frames.find((frame) => frame.paint.seq === seq);
const record = (seq, frame) => ({ v: 1, kind: "data", seq, bytes: frame.paint.bytes });

async function startSampler(page) {
  await page.evaluate(() => {
    window.blankPaintSamples = [];
    window.blankPaintSampling = true;
    function sample() {
      if (!window.blankPaintSampling) return;
      const grids = [...document.querySelectorAll(".peek-terminal .xterm-rows")].filter((grid) => {
        const style = getComputedStyle(grid);
        return style.visibility === "visible" && style.opacity !== "0";
      });
      window.blankPaintSamples.push({ at: performance.now(), grids: grids.map((grid) => grid.textContent.trim()) });
      requestAnimationFrame(sample);
    }
    requestAnimationFrame(sample);
  });
}

const visibleText = (page) => page.locator(".peek-terminal .xterm-rows:visible").allTextContents().then((rows) => rows.join("\n").trim());

test("recorded native 736/737 never publishes the intervening blank grid", { timeout: 30000 }, async (t) => {
  for (const seq of [736, 737]) {
    const frame = native(seq);
    const bytes = Buffer.from(frame.paint.bytes, "base64");
    assert.equal(createHash("sha256").update(bytes).digest("hex"), frame.sha256);
    assert.equal(bytes.length, seq === 736 ? 754 : 1062);
  }
  const browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/chromium", headless: true, args: ["--no-sandbox"] });
  const ui = await open(browser, t, full(1, { cols: 120, rows: 40, bytes: encode(`\x1b[2J${seed}`) }));
  try {
    await clickMember(ui, "codex");
    await until(async () => (await visibleText(ui.page)).includes("PROSE_COMPLETE"), "native 735 seeded grid");
    const beforeHistory = await ui.page.locator(".peek-history span").allTextContents();
    await startSampler(ui.page);
    ui.live().send(record(2, native(736)));
    await new Promise((resolve) => setTimeout(resolve, Math.round((native(737).at - native(736).at) * 1000)));
    ui.live().send(record(3, native(737)));
    await until(async () => (await visibleText(ui.page)).includes("148."), "native 737 rendered redraw");
    await ui.page.waitForTimeout(160);
    const samples = await ui.page.evaluate(() => { window.blankPaintSampling = false; return window.blankPaintSamples; });
    assert.ok(samples.length >= 4, "actual browser animation frames observed");
    const blank = samples.filter((sample) => sample.grids.length !== 1 || !sample.grids[0]);
    assert.deepEqual(blank, [], "no displayed blank grid between recorded 736 clear and 737 redraw");
    assert.deepEqual(await ui.page.locator(".peek-history span").allTextContents(), beforeHistory, "held blank and redraw do not invent history");
    assert.deepEqual(ui.errors, []);
  } finally {
    await ui.close();
    await browser.close();
  }
});
