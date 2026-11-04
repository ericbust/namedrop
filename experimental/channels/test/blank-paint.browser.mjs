import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium } from "playwright-core";
import { clickMember, data, full, open, until } from "./live-peek.browser.mjs";
import { withPresentation } from "./peek-presentation.mjs";

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

test("blank presentation reset cancels a suspended frame and ignores its stale callback", {
  timeout: 15000,
}, async () => {
  await withPresentation(async (page) => {
    await page.evaluate(() => {
      void window.probe
        .apply("data", "STALE")
        .then(() => window.probe.settled.push("old"));
    });
    await page.waitForFunction(() => window.frameGate.requested > 0);
    await page.evaluate(() => {
      void window.probe
        .apply("reset", "RESET SURVIVES")
        .then(() => window.probe.settled.push("reset"));
    });
    await page.waitForTimeout(100);
    assert.equal(
      await page.evaluate(() => window.probe.settled.includes("old")),
      true,
      "reset releases the old publication while animation frames stay suspended",
    );
    assert.ok(
      await page.evaluate(() => window.frameGate.cancelled.length > 0),
      "reset cancels the suspended platform frame",
    );
    await page.evaluate(() => window.frameGate.release());
    await page.waitForFunction(() => window.probe.settled.includes("reset"));
    await page.evaluate(() => window.frameGate.deliverStale());
    assert.deepEqual(
      await page.evaluate(() => window.probe.commits),
      ["INITIAL", "RESET SURVIVES"],
      "cancelled frame cannot commit old content after reset",
    );
  });
});

test("blank presentation disposal settles a suspended publication", {
  timeout: 15000,
}, async () => {
  await withPresentation(async (page) => {
    await page.evaluate(() => {
      void window.probe
        .apply("data", "DISPOSED")
        .then(() => window.probe.settled.push("disposed"));
    });
    await page.waitForFunction(() => window.frameGate.requested > 0);
    await page.evaluate(() => window.probe.presentation.dispose());
    await page.waitForTimeout(100);
    assert.equal(
      await page.evaluate(() => window.probe.settled.includes("disposed")),
      true,
      "disposal settles apply without a future animation frame",
    );
    assert.ok(
      await page.evaluate(() => window.frameGate.cancelled.length > 0),
      "disposal cancels the suspended platform frame",
    );
    await page.evaluate(() => window.frameGate.deliverStale());
    assert.deepEqual(await page.evaluate(() => window.probe.commits), [
      "INITIAL",
    ]);
  });
});

test("blank presentation settles suspended font rendering on reset disposal and hiding", {
  timeout: 30000,
}, async () => {
  for (const action of ["reset", "dispose", "hidden"]) {
    await withPresentation(async (page) => {
      await page.evaluate(() => {
        void window.probe.presentation
          .setFontSize(18)
          .then(() => window.probe.settled.push("font"));
      });
      await page.waitForFunction(() => window.frameGate.requested > 0);
      await page.evaluate((action) => {
        if (action === "reset") {
          void window.probe
            .apply("reset", "RESET AFTER FONT")
            .then(() => window.probe.settled.push("reset"));
        } else if (action === "dispose") window.probe.presentation.dispose();
        else window.frameGate.visibility("hidden");
      }, action);
      await page.waitForTimeout(100);
      assert.equal(
        await page.evaluate(() => window.probe.settled.includes("font")),
        true,
        `${action} settles font rendering without an animation frame`,
      );
      if (action === "hidden") {
        await page.evaluate(async () => {
          await window.probe.apply("data", "HIDDEN AFTER FONT");
          window.probe.settled.push("hidden");
        });
        assert.equal(
          await page.evaluate(() => window.probe.settled.includes("hidden")),
          true,
          "hidden source ingestion progresses after cancelling a font render wait",
        );
      } else if (action === "reset") {
        await page.evaluate(() => window.frameGate.release());
        await page.waitForFunction(() =>
          window.probe.settled.includes("reset"),
        );
        assert.equal(
          await page.evaluate(() => window.probe.commits.at(-1)),
          "RESET AFTER FONT",
        );
      }
    });
  }
});

test("blank presentation defers font-only swaps across a held clear and source transaction", {
  timeout: 20000,
}, async () => {
  for (const kind of ["clear", "transaction"]) {
    await withPresentation(async (page) => {
      await page.evaluate(async (kind) => {
        window.frameGate.release();
        window.probe.originalFont =
          window.probe.presentation.visible.options.fontSize;
        await window.probe.presentation.apply({
          kind: "data",
          bytes: new TextEncoder().encode(
            `${kind === "transaction" ? "\x1b[?2026h" : ""}\x1b[2J\x1b[H`,
          ),
        });
        await window.probe.presentation.setFontSize(18);
      }, kind);
      assert.deepEqual(
        await page.evaluate(() => window.probe.commits),
        ["INITIAL"],
        `${kind} keeps the previous completed drawing while font work is pending`,
      );
      assert.equal(
        await page.evaluate(
          () =>
            window.probe.presentation.visible.options.fontSize ===
            window.probe.originalFont,
        ),
        true,
        "deferred font work leaves the visible renderer untouched",
      );
      if (kind === "transaction") {
        await page.evaluate(async () => {
          await window.probe.presentation.apply({
            kind: "data",
            bytes: new TextEncoder().encode("TRANSACTION COMPLETE\x1b[?2026l"),
          });
        });
      } else {
        await page.waitForFunction(() => window.probe.commits.at(-1) === "");
      }
      assert.deepEqual(
        await page.evaluate(() => window.probe.commits),
        ["INITIAL", kind === "transaction" ? "TRANSACTION COMPLETE" : ""],
        "only the completed source boundary publishes, without an intermediate font-only frame",
      );
      assert.equal(
        await page.evaluate(
          () => window.probe.presentation.visible.options.fontSize,
        ),
        18,
        "the completed source publication includes the deferred font size",
      );
    });
  }
});

test("blank presentation ingests hidden frames and refreshes the latest grid on return", {
  timeout: 15000,
}, async () => {
  await withPresentation(async (page) => {
    await page.evaluate(() => {
      void (async () => {
        await window.probe.apply("data", "HIDDEN FIRST");
        await window.probe.presentation.apply({
          kind: "data",
          bytes: new TextEncoder().encode("\x1b[?2026h\x1b[2J\x1b[H"),
        });
        window.probe.settled.push("transaction");
      })();
    });
    await page.waitForFunction(() => window.frameGate.requested > 0);
    await page.evaluate(() => window.frameGate.visibility("hidden"));
    await page.waitForTimeout(100);
    assert.equal(
      await page.evaluate(() => window.probe.settled.includes("transaction")),
      true,
      "hiding releases an already suspended publication and accepts an open transaction",
    );
    assert.ok(
      await page.evaluate(() => window.frameGate.cancelled.length > 0),
      "hiding cancels the suspended platform frame",
    );
    assert.deepEqual(
      await page.evaluate(() => window.probe.commits),
      ["INITIAL", "HIDDEN FIRST"],
      "hidden ingestion preserves source synchronized output boundaries",
    );
    await page.evaluate(() => {
      void (async () => {
        await window.probe.presentation.apply({
          kind: "data",
          bytes: new TextEncoder().encode("\x1b[?2026l"),
        });
        await new Promise((resolve) => setTimeout(resolve, 65));
        window.probe.beforeClear = [...window.probe.commits];
        await new Promise((resolve) => setTimeout(resolve, 100));
        await window.probe.apply("data", "HIDDEN LATEST");
        window.probe.settled.push("hidden");
      })();
    });
    await page.waitForTimeout(250);
    assert.equal(
      await page.evaluate(() => window.probe.settled.includes("hidden")),
      true,
      "healthy hidden ingestion completes while physical frames stay suspended",
    );
    assert.deepEqual(
      await page.evaluate(() => window.probe.commits),
      ["INITIAL", "HIDDEN FIRST", "", "HIDDEN LATEST"],
      "a completed isolated clear commits at its deadline without blocking later hidden input",
    );
    assert.deepEqual(
      await page.evaluate(() => window.probe.beforeClear),
      ["INITIAL", "HIDDEN FIRST"],
      "hidden clear still holds the prior content before its deadline",
    );
    await page.evaluate(() => {
      window.frameGate.visibility("visible");
      window.frameGate.release();
      window.probe.returned = [];
      function sample() {
        window.probe.returned.push(
          [
            ...document.querySelectorAll(".peek-buffer-visible .xterm-rows"),
          ].map((grid) => grid.textContent.trim()),
        );
        if (window.probe.returned.length < 3) requestAnimationFrame(sample);
      }
      requestAnimationFrame(sample);
    });
    await page.waitForFunction(() => window.probe.returned.length === 3);
    assert.deepEqual(
      await page.evaluate(() => window.probe.returned),
      [["HIDDEN LATEST"], ["HIDDEN LATEST"], ["HIDDEN LATEST"]],
      "the return frames show only the latest completed hidden grid",
    );
    assert.equal(
      (
        await page.locator(".peek-buffer-visible .xterm-rows").textContent()
      ).trim(),
      "HIDDEN LATEST",
      "public xterm refresh restores the latest hidden grid",
    );
  });
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

async function withRecordedTerminal(t, run) {
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
      "recorded initial screen rendered",
    );
    await run(ui);
    assert.deepEqual(ui.errors, []);
    assert.deepEqual(
      ui.requests.filter((request) => request.method === "POST"),
      [],
      "focus and draft editing never send input",
    );
  } finally {
    await ui.close();
    await browser.close();
  }
}

test("blank presentation keeps idle fitted content through width Expand and Restore changes", {
  timeout: 30000,
}, async (t) => {
  await withRecordedTerminal(t, async (ui) => {
    const copyMarker = async () => {
      const marker = ui.page
        .locator(".peek-buffer-visible .xterm-rows > div")
        .nth(8);
      await marker.scrollIntoViewIfNeeded();
      const markerBox = await marker.boundingBox();
      await ui.page.mouse.dblclick(
        markerBox.x + (markerBox.width * 9) / 120,
        markerBox.y + markerBox.height / 2,
      );
      return ui.page.evaluate(() => {
        const clipboardData = new DataTransfer();
        document.activeElement.dispatchEvent(
          new ClipboardEvent("copy", { clipboardData, bubbles: true }),
        );
        return clipboardData.getData("text/plain").trim();
      });
    };
    assert.equal(
      await copyMarker(),
      "PROSE_COMPLETE",
      "initial fitted terminal selection copies the displayed word",
    );
    const extent = await ui.page.locator(".peek-live").evaluate((element) => {
      const terminal = element
        .querySelector(".peek-terminal")
        .getBoundingClientRect();
      const scroller = element.getBoundingClientRect();
      const padding = parseFloat(getComputedStyle(element).paddingBottom);
      return {
        actual: element.scrollHeight,
        expected: Math.max(
          element.clientHeight,
          terminal.bottom - scroller.top + element.scrollTop + padding,
        ),
      };
    });
    assert.ok(
      Math.abs(extent.actual - extent.expected) <= 2,
      `the fitted projection owns its scroll extent: ${JSON.stringify(extent)}`,
    );
    ui.live().send({
      v: 1,
      kind: "history",
      seq: 2,
      text: Array.from({ length: 260 }, (_, i) => `IDLE HISTORY ${i}`).join(
        "\n",
      ),
    });
    await ui.page.locator(".peek-history span").nth(259).waitFor();
    const history = await ui.page
      .locator(".peek-history span")
      .allTextContents();
    const intersections = await ui.page.evaluate(async () => {
      document.querySelector(".peek-live").scrollTop = 0;
      const stage = document.querySelector(
        ".peek-buffer:not(.peek-buffer-visible)",
      );
      const visible = document.querySelector(".peek-buffer-visible");
      const observe = (element) =>
        new Promise((resolve) => {
          const observer = new IntersectionObserver(([entry]) => {
            observer.disconnect();
            resolve(entry.isIntersecting);
          });
          observer.observe(element);
        });
      return { stage: await observe(stage), visible: await observe(visible) };
    });
    assert.deepEqual(
      intersections,
      { stage: true, visible: false },
      "staging stays drawable while the reader views history offscreen",
    );
    await ui.page.evaluate(() => {
      window.fitSamples = [];
      window.fitSampling = true;
      function sample() {
        if (!window.fitSampling) return;
        const scroller = document.querySelector(".peek-live");
        const box = scroller.getBoundingClientRect();
        const grids = [
          ...document.querySelectorAll(".peek-buffer-visible .xterm-rows"),
        ];
        window.fitSamples.push(
          grids.map((grid) => {
            const rect = grid.getBoundingClientRect();
            return {
              text: grid.textContent.trim(),
              intersecting: rect.bottom > box.top && rect.top < box.bottom,
              width: rect.width,
            };
          }),
        );
        requestAnimationFrame(() => setTimeout(sample, 0));
      }
      requestAnimationFrame(() => setTimeout(sample, 0));
    });
    const widths = [];
    for (const width of [960, 1700, 1280]) {
      await ui.page.setViewportSize({ width, height: 1000 });
      for (const button of ["Expand", "Restore"]) {
        await ui.drawer
          .getByRole("button", { name: button, exact: true })
          .click();
        await ui.page.waitForTimeout(100);
        await ui.page.locator(".peek-live").focus();
        await ui.page.keyboard.press("Control+End");
        await ui.page.waitForTimeout(150);
        widths.push(
          await ui.page.locator(".peek-live").evaluate((el) => {
            const style = getComputedStyle(el);
            return {
              available:
                el.clientWidth -
                parseFloat(style.paddingLeft) -
                parseFloat(style.paddingRight),
              screen: el
                .querySelector(".peek-buffer-visible .xterm-screen")
                .getBoundingClientRect().width,
              font: parseFloat(
                getComputedStyle(el.querySelector(".peek-history pre"))
                  .fontSize,
              ),
              visualFont:
                parseFloat(
                  getComputedStyle(
                    el.querySelector(".peek-buffer-visible .xterm-rows"),
                  ).fontSize,
                ) *
                parseFloat(
                  getComputedStyle(el.querySelector(".peek-terminal")).zoom,
                ),
            };
          }),
        );
        await ui.page
          .locator(".peek-history span")
          .nth(200)
          .evaluate((line) => {
            line.closest(".peek-live").scrollTop = line.offsetTop;
          });
        await ui.page.waitForTimeout(100);
      }
    }
    const observations = await ui.page.evaluate(() => {
      window.fitSampling = false;
      return window.fitSamples;
    });
    assert.ok(
      observations.length > 20,
      "real browser frames observed while fitting idle content",
    );
    assert.ok(
      observations.some((grids) => grids.some((grid) => grid.intersecting)),
      "the idle terminal is actually displayed during fit actions",
    );
    assert.deepEqual(
      observations.filter((grids) =>
        grids.some((grid) => grid.intersecting && !grid.text),
      ),
      [],
      "idle width and Expand/Restore changes never display an empty source grid",
    );
    assert.ok(
      Math.max(...widths.map((fit) => fit.screen)) >
        Math.min(...widths.map((fit) => fit.screen)) + 100,
      "Expand changes the actual projected terminal width",
    );
    for (const fit of widths) {
      assert.ok(fit.font >= 10, "fit keeps text readable");
      assert.ok(
        fit.visualFont >= 9.99 && fit.visualFont <= 20.01,
        "rendered terminal text stays between the readability floor and 20 px cap",
      );
      assert.ok(
        Math.abs(fit.visualFont - fit.font) < 0.1,
        "history and projected terminal text use matching visual sizes",
      );
      assert.ok(
        fit.screen <= fit.available + 2 || fit.font === 10,
        `fit uses available width or the readability floor: ${JSON.stringify(fit)}`,
      );
    }
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      history,
      "idle fitting never changes retained history",
    );
    assert.equal(
      ui.liveCount(),
      1,
      "fitting retains the same idle live stream",
    );
    assert.equal(
      await copyMarker(),
      "PROSE_COMPLETE",
      "selection follows the fitted terminal geometry",
    );
  });
});

test("blank presentation keeps terminal focus across every ordinary buffer swap", {
  timeout: 30000,
}, async (t) => {
  await withRecordedTerminal(t, async (ui) => {
    await ui.page.locator(".peek-buffer-visible .xterm-screen").click();
    const focus = () =>
      ui.page.evaluate(() => ({
        tag: document.activeElement.tagName,
        visible:
          document.activeElement
            .closest(".peek-buffer")
            ?.getAttribute("aria-hidden") === "false",
        textareas: document.querySelectorAll(".peek-buffer-visible textarea")
          .length,
      }));
    const expected = { tag: "TEXTAREA", visible: true, textareas: 1 };
    assert.deepEqual(
      await focus(),
      expected,
      "visible terminal starts focused",
    );
    for (let seq = 2; seq <= 5; seq++) {
      const marker = `ORDINARY UPDATE ${seq}`;
      ui.live().send(data(seq, `\x1b[1;1H\x1b[2K${marker}`));
      await until(
        async () => (await visibleText(ui.page)).includes(marker),
        `ordinary update ${seq} rendered`,
      );
      assert.deepEqual(
        await focus(),
        expected,
        `terminal focus survives update ${seq}`,
      );
    }
  });
});

test("blank presentation preserves outside focus and composer selection during swaps", {
  timeout: 30000,
}, async (t) => {
  for (const target of ["body", "composer", "history", "button"]) {
    await withRecordedTerminal(t, async (ui) => {
      let focused;
      if (target === "composer") {
        await ui.drawer.getByRole("button", { name: "Type to agent" }).click();
        focused = ui.drawer.getByRole("combobox", {
          name: "Message to @codex",
        });
        await focused.fill("Keep this unsent draft");
        await focused.evaluate((field) => field.setSelectionRange(5, 9));
      } else if (target === "history") {
        focused = ui.page.locator(".peek-live");
        await focused.focus();
      } else if (target === "button") {
        focused = ui.drawer.getByRole("button", { name: "Close screen" });
        await focused.focus();
      } else {
        await ui.page.evaluate(() => document.activeElement.blur());
        focused = ui.page.locator("body");
      }
      for (let seq = 2; seq <= 4; seq++) {
        const marker = `OUTSIDE FOCUS ${seq}`;
        ui.live().send(data(seq, `\x1b[1;1H\x1b[2K${marker}`));
        await until(
          async () => (await visibleText(ui.page)).includes(marker),
          `outside focus update ${seq} rendered`,
        );
        assert.equal(
          await focused.evaluate(
            (element) => document.activeElement === element,
          ),
          true,
          `${target} keeps focus during update ${seq}`,
        );
        if (target === "composer") {
          assert.deepEqual(
            await focused.evaluate((field) => ({
              text: field.value,
              start: field.selectionStart,
              end: field.selectionEnd,
            })),
            { text: "Keep this unsent draft", start: 5, end: 9 },
            "draft and text selection survive terminal swaps",
          );
        }
      }
    });
  }
});

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
    await ui.page.setViewportSize({ width: 320, height: 900 });
    await ui.page.waitForTimeout(160);
    assert.equal(
      await visibleText(ui.page),
      "COMPLETE BEFORE",
      "open transaction cannot publish its clear",
    );
    ui.live().send(data(3, "PARTIAL CONTENT"));
    await ui.page.setViewportSize({ width: 1440, height: 900 });
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
