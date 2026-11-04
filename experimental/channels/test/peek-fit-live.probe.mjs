import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { chromium } from "playwright-core";

const root = process.env.PEEK_FIT_LIVE_OUTPUT;
await mkdir(root, { recursive: true });
const launch = JSON.parse(
  await readFile(process.env.PEEK_FIT_DASHBOARD, "utf8"),
);
const browser = await chromium.launch({
  executablePath: "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
});
const page = await context.newPage();
const observations = [];
try {
  await page.goto(launch.data.url);
  await page.locator(".member").first().waitFor();
  for (const alias of ["@claude", "@codex"]) {
    const chip = page.locator(".member").filter({ hasText: alias }).first();
    await chip.click();
    await page.locator(".peek-live .xterm-rows > div").first().waitFor();
    await page.waitForTimeout(300);
    for (const [width, height] of [
      [1280, 800],
      [1920, 1080],
    ]) {
      await page.setViewportSize({ width, height });
      for (const expanded of [false, true]) {
        const restore = page.getByRole("button", {
          name: "Restore",
          exact: true,
        });
        if (Boolean(await restore.count()) !== expanded)
          await page
            .getByRole("button", {
              name: expanded ? "Expand" : "Restore",
              exact: true,
            })
            .click();
        for (const compose of [false, true]) {
          if (compose)
            await page
              .getByRole("button", { name: "Type to agent", exact: true })
              .click();
          await page.waitForTimeout(200);
          const geometry = await page.locator(".peek-live").evaluate((el) => {
            const rect = el.getBoundingClientRect();
            const rows = [...el.querySelectorAll(".xterm-rows > div")];
            const lastText = rows.findLast((row) => row.textContent.trim());
            const last = lastText?.getBoundingClientRect();
            const terminal = el
              .querySelector(".xterm-screen")
              .getBoundingClientRect();
            const footer = document
              .querySelector(".peek-compose")
              .getBoundingClientRect();
            return {
              frameBottom: rect.bottom,
              frameTop: rect.top,
              footerTop: footer.top,
              footerBottom: footer.bottom,
              lastTop: last?.top,
              lastBottom: last?.bottom,
              lastText: lastText?.textContent,
              gridWidth: terminal.width,
              frameWidth: el.clientWidth,
              scrollTop: el.scrollTop,
              rowCount: rows.length,
              text: rows.map((row) => row.textContent).join("\n"),
            };
          });
          const name = `${alias.slice(1)}-${width}x${height}-${expanded ? "expanded" : "normal"}-${compose ? "compose" : "closed"}`;
          observations.push({ name, ...geometry });
          await page.screenshot({ path: join(root, `${name}.png`) });
          assert.ok(
            Math.abs(geometry.frameBottom - geometry.footerTop) <= 1,
            `${name}: frame reaches footer`,
          );
          assert.ok(
            Math.abs(geometry.footerBottom - height) <= 1,
            `${name}: footer reaches window bottom`,
          );
          assert.ok(
            geometry.lastTop >= geometry.frameTop &&
              geometry.lastBottom <= geometry.frameBottom + 1,
            `${name}: newest populated row stays visible`,
          );
          if (compose)
            await page
              .getByRole("button", { name: "Stop typing", exact: true })
              .click();
        }
      }
    }
    await page
      .getByRole("button", { name: "Close screen", exact: true })
      .click();
  }
} finally {
  await writeFile(
    join(root, "geometry.json"),
    JSON.stringify(observations, null, 2),
  );
  await browser.close();
}
