import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium } from "playwright-core";
import { clickMember, data, full, open, until } from "./live-peek.browser.mjs";

const fixture = JSON.parse(
  await readFile(
    new URL("./fixtures/blank-736-737.json", import.meta.url),
    "utf8",
  ),
);
const encode = (value) => Buffer.from(value).toString("base64");
const seed = fixture.seed.text
  .split("\n")
  .map((row, i) => `\x1b[${i + 1};1H${row}`)
  .join("");
const native = (seq) => fixture.frames.find((frame) => frame.paint.seq === seq);
const record = (seq, frame) => ({
  v: 1,
  kind: "data",
  seq,
  bytes: frame.paint.bytes,
});

async function startSampler(page) {
  await page.evaluate(() => {
    window.blankPaintSamples = [];
    window.blankPaintSampling = true;
    function sample() {
      if (!window.blankPaintSampling) return;
      const grids = [
        ...document.querySelectorAll(".peek-terminal .xterm-rows"),
      ].filter((grid) => {
        const style = getComputedStyle(grid);
        return style.visibility === "visible" && style.opacity !== "0";
      });
      window.blankPaintSamples.push({
        at: performance.now(),
        grids: grids.map((grid) => grid.textContent.trim()),
      });
      requestAnimationFrame(sample);
    }
    requestAnimationFrame(sample);
  });
}

const visibleText = (page) =>
  page
    .locator(".peek-terminal .xterm-rows:visible")
    .allTextContents()
    .then((rows) => rows.join("\n").trim());

test("recorded native 736/737 never publishes the intervening blank grid", {
  timeout: 30000,
}, async (t) => {
  for (const seq of [736, 737]) {
    const frame = native(seq);
    const bytes = Buffer.from(frame.paint.bytes, "base64");
    assert.equal(
      createHash("sha256").update(bytes).digest("hex"),
      frame.sha256,
    );
    assert.equal(bytes.length, seq === 736 ? 754 : 1062);
  }
  const browser = await chromium.launch({
    executablePath: process.env.CHROME ?? "/usr/bin/chromium",
    headless: true,
    args: ["--no-sandbox"],
  });
  const ui = await open(
    browser,
    t,
    full(1, { cols: 120, rows: 40, bytes: encode(`\x1b[2J${seed}`) }),
  );
  try {
    await clickMember(ui, "codex");
    await until(
      async () => (await visibleText(ui.page)).includes("PROSE_COMPLETE"),
      "native 735 seeded grid",
    );
    const beforeHistory = await ui.page
      .locator(".peek-history span")
      .allTextContents();
    await startSampler(ui.page);
    ui.live().send(record(2, native(736)));
    await new Promise((resolve) =>
      setTimeout(resolve, Math.round((native(737).at - native(736).at) * 1000)),
    );
    ui.live().send(record(3, native(737)));
    await until(
      async () => (await visibleText(ui.page)).includes("148."),
      "native 737 rendered redraw",
    );
    await ui.page.waitForTimeout(160);
    const samples = await ui.page.evaluate(() => {
      window.blankPaintSampling = false;
      return window.blankPaintSamples;
    });
    assert.ok(samples.length >= 4, "actual browser animation frames observed");
    const blank = samples.filter(
      (sample) => sample.grids.length !== 1 || !sample.grids[0],
    );
    assert.deepEqual(
      blank,
      [],
      "no displayed blank grid between recorded 736 clear and 737 redraw",
    );
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      beforeHistory,
      "held blank and redraw do not invent history",
    );
    assert.deepEqual(ui.errors, []);
  } finally {
    await ui.close();
    await browser.close();
  }
});

async function withTerminal(t, text, run) {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME ?? "/usr/bin/chromium",
    headless: true,
    args: ["--no-sandbox"],
  });
  const ui = await open(
    browser,
    t,
    full(1, { cols: 40, rows: 10, bytes: encode(`\x1b[2J\x1b[H${text}`) }),
  );
  try {
    await clickMember(ui, "codex");
    await until(
      async () => (await visibleText(ui.page)) === text,
      "initial rendered grid",
    );
    await run(ui);
    assert.deepEqual(ui.errors, []);
  } finally {
    await ui.close();
    await browser.close();
  }
}

const clear = "\x1b[?2026h\x1b[2J\x1b[H\x1b[?2026l";
const samples = (page) =>
  page.evaluate(() => {
    window.blankPaintSampling = false;
    return window.blankPaintSamples;
  });

test("blank presentation keeps the first deadline across consecutive clears", {
  timeout: 30000,
}, async (t) => {
  await withTerminal(t, "VISIBLE CONTENT", async (ui) => {
    await startSampler(ui.page);
    const start = await ui.page.evaluate(() => performance.now());
    ui.live().send(data(2, clear));
    await ui.page.waitForTimeout(65);
    assert.equal(
      await visibleText(ui.page),
      "VISIBLE CONTENT",
      "clear stays held before deadline",
    );
    ui.live().send(data(3, clear));
    await until(
      async () => (await visibleText(ui.page)) === "",
      "actual isolated clear published",
    );
    await ui.page.waitForFunction(() =>
      window.blankPaintSamples.some(
        (sample) => sample.grids.length === 1 && !sample.grids[0],
      ),
    );
    const observations = await samples(ui.page);
    const blank = observations.find(
      (sample) => sample.grids.length === 1 && !sample.grids[0],
    );
    assert.ok(blank, "actual rendered clear observed");
    assert.ok(
      blank.at - start >= 110,
      "clear is held through the 120ms deadline",
    );
    assert.ok(
      blank.at - start < 190,
      "consecutive blank paint does not restart the first deadline",
    );
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      [],
    );
    ui.live().send(data(4, "\x1b[HAFTER CLEAR"));
    await until(
      async () => (await visibleText(ui.page)) === "AFTER CLEAR",
      "meaningful successor rendered",
    );
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      [],
      "clear does not become a false scroll baseline",
    );
  });
});

test("blank presentation shows a genuinely empty initial screen", {
  timeout: 30000,
}, async (t) => {
  await withTerminal(t, "", async (ui) => {
    await ui.page
      .locator(".peek-watching")
      .filter({ hasText: /^Watching since/ })
      .waitFor();
    assert.equal(
      await ui.page.locator(".peek-terminal .xterm-rows:visible").count(),
      1,
    );
    assert.equal(await visibleText(ui.page), "");
  });
});

test("blank presentation invalidates stale deadlines on reset reconnect and disposal", {
  timeout: 30000,
}, async (t) => {
  await withTerminal(t, "ORIGINAL", async (ui) => {
    await startSampler(ui.page);
    ui.live().send(data(2, clear));
    await ui.page.waitForTimeout(40);
    ui.live().send(full(3, { bytes: encode("\x1b[2J\x1b[HRESET SURVIVES") }));
    await until(
      async () => (await visibleText(ui.page)) === "RESET SURVIVES",
      "reset replaces held clear",
    );
    await ui.page.waitForTimeout(160);
    assert.equal(await visibleText(ui.page), "RESET SURVIVES");
    assert.deepEqual(
      (await samples(ui.page)).filter(
        (sample) => sample.grids.length !== 1 || !sample.grids[0],
      ),
      [],
      "stale reset callback never renders a clear",
    );
    ui.live().send(data(4, clear));
    await ui.page.waitForTimeout(30);
    const old = ui.live();
    old.res.end();
    await ui.page.waitForTimeout(200);
    assert.equal(
      await visibleText(ui.page),
      "RESET SURVIVES",
      "reconnect cancels the old clear deadline",
    );
    await until(() => ui.liveCount() === 2, "transport reconnect");
    await until(
      async () => (await visibleText(ui.page)) === "ORIGINAL",
      "fresh reconnect reset",
    );
    ui.live().send(data(2, clear));
    await ui.page.waitForTimeout(30);
    await ui.drawer.getByRole("button", { name: "Close screen" }).click();
    await ui.drawer.waitFor({ state: "detached" });
    await clickMember(ui, "codex");
    await until(
      async () => (await visibleText(ui.page)) === "ORIGINAL",
      "fresh terminal after disposal",
    );
    await ui.page.waitForTimeout(160);
    assert.equal(
      await visibleText(ui.page),
      "ORIGINAL",
      "disposed presentation cannot publish its held clear",
    );
  });
});

test("blank presentation never exposes split synchronized transactions or 64KiB reset chunks", {
  timeout: 30000,
}, async (t) => {
  await withTerminal(t, "COMPLETE BEFORE", async (ui) => {
    await startSampler(ui.page);
    ui.live().send(data(2, "\x1b[?2026h\x1b[2J\x1b[H"));
    await ui.page.waitForTimeout(160);
    assert.equal(
      await visibleText(ui.page),
      "COMPLETE BEFORE",
      "open transaction cannot publish its clear",
    );
    ui.live().send(data(3, "PARTIAL CONTENT"));
    await ui.page.waitForTimeout(40);
    assert.equal(
      await visibleText(ui.page),
      "COMPLETE BEFORE",
      "open transaction cannot publish partial content",
    );
    ui.live().send(data(4, "\r\x1b[2KCOMPLETE AFTER\x1b[?2026l"));
    await until(
      async () => (await visibleText(ui.page)) === "COMPLETE AFTER",
      "closed transaction rendered",
    );
    const large = `\x1b[?2026h\x1b[2J\x1b[HPARTIAL RESET\x1b]0;${"x".repeat(75000)}\x07\r\x1b[2KCOMPLETE RESET\x1b[?2026l`;
    ui.live().send(full(5, { bytes: encode(large) }));
    await until(
      async () => (await visibleText(ui.page)) === "COMPLETE RESET",
      "large split reset rendered",
    );
    const observations = await samples(ui.page);
    assert.deepEqual(
      observations.filter(
        (sample) =>
          sample.grids.length !== 1 ||
          !["COMPLETE BEFORE", "COMPLETE AFTER", "COMPLETE RESET"].includes(
            sample.grids[0],
          ),
      ),
      [],
      "every browser frame is one completed transaction",
    );
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      [],
    );
  });
});

test("blank presentation publishes an isolated clear at 120ms without accepting history", {
  timeout: 30000,
}, async (t) => {
  await withTerminal(t, "VISIBLE UNTIL CLEAR", async (ui) => {
    await startSampler(ui.page);
    const start = await ui.page.evaluate(() => performance.now());
    ui.live().send(data(2, clear));
    await ui.page.waitForTimeout(70);
    assert.equal(await visibleText(ui.page), "VISIBLE UNTIL CLEAR");
    await ui.page.waitForFunction(() =>
      window.blankPaintSamples.some(
        (sample) => sample.grids.length === 1 && !sample.grids[0],
      ),
    );
    const blank = (await samples(ui.page)).find(
      (sample) => sample.grids.length === 1 && !sample.grids[0],
    );
    assert.ok(
      blank.at - start >= 110 && blank.at - start < 200,
      "actual clear publishes at the 120ms deadline plus the render frame",
    );
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      [],
    );
    ui.live().send(data(3, clear));
    await ui.page.waitForTimeout(40);
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      [],
      "repeated empty frames do not establish history after the held clear",
    );
  });
});
