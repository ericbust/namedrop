import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright-core";
import { createServer } from "vite";

export async function withPresentation(run) {
  const cacheDir = await mkdtemp(join(tmpdir(), "peek-presentation-"));
  const server = await createServer({
    configFile: false,
    root: new URL("../", import.meta.url).pathname,
    cacheDir,
    optimizeDeps: { noDiscovery: true },
    server: { host: "127.0.0.1", port: 0 },
    logLevel: "error",
  });
  const browser = await chromium.launch({
    executablePath: process.env.CHROME ?? "/usr/bin/chromium",
    headless: true,
    args: ["--no-sandbox"],
  });
  try {
    await server.listen();
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("**/probe.html", (route) =>
      route.fulfill({
        contentType: "text/html",
        body: '<link rel="stylesheet" href="/node_modules/@xterm/xterm/css/xterm.css"><link rel="stylesheet" href="/src/style.css"><div class="peek-terminal"></div>',
      }),
    );
    await page.goto(`${server.resolvedUrls.local[0]}probe.html`);
    await page.evaluate(async () => {
      const { PeekPresentation } = await import("/src/peekPresentation.ts");
      const { Terminal } = await import(
        "/node_modules/@xterm/xterm/lib/xterm.mjs"
      );
      const text = (terminal) => {
        const buffer = terminal.buffer.active;
        return Array.from(
          { length: terminal.rows },
          (_, y) =>
            buffer.getLine(buffer.baseY + y)?.translateToString(true) ?? "",
        )
          .join("\n")
          .trim();
      };
      const commits = [];
      const presentation = new PeekPresentation(
        document.querySelector(".peek-terminal"),
        (cols, rows) => new Terminal({ cols, rows }),
        ({ terminal }) => commits.push(text(terminal)),
      );
      const apply = (kind, content) =>
        presentation.apply({
          kind,
          bytes: new TextEncoder().encode(`\x1b[2J\x1b[H${content}`),
          ...(kind === "reset"
            ? { viewport: { cols: 40, rows: 10 }, continuity: "probe" }
            : {}),
        });
      window.probe = { presentation, apply, commits, settled: [] };
      await apply("reset", "INITIAL");
    });
    await page.waitForFunction(() =>
      document
        .querySelector(".peek-buffer-visible .xterm-rows")
        ?.textContent.includes("INITIAL"),
    );
    await page.evaluate(() => {
      const request = window.requestAnimationFrame.bind(window);
      const cancel = window.cancelAnimationFrame.bind(window);
      const callbacks = new Map();
      const captured = [];
      let next = 1000000;
      let withheld = true;
      window.frameGate = {
        requested: 0,
        cancelled: [],
        visibility(state) {
          Object.defineProperty(document, "visibilityState", {
            configurable: true,
            get: () => state,
          });
          document.dispatchEvent(new Event("visibilitychange"));
        },
        release() {
          withheld = false;
          for (const callback of callbacks.values()) request(callback);
          callbacks.clear();
        },
        deliverStale() {
          for (const callback of captured) callback(performance.now());
        },
      };
      window.requestAnimationFrame = (callback) => {
        if (!withheld) return request(callback);
        const id = next++;
        callbacks.set(id, callback);
        captured.push(callback);
        window.frameGate.requested++;
        return id;
      };
      window.cancelAnimationFrame = (id) => {
        if (callbacks.delete(id)) window.frameGate.cancelled.push(id);
        else cancel(id);
      };
    });
    await run(page);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await server.close();
    await rm(cacheDir, { recursive: true, force: true });
  }
}
