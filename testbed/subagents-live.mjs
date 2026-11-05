import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(join(root, "experimental/channels/package.json"));
const { chromium } = require("playwright-core");
const [url, output, alias] = process.argv.slice(2);
assert.ok(url && output && ["claude", "codex"].includes(alias));
assert.equal(new URL(url).hostname, "127.0.0.1");
const browser = await chromium.launch({
  executablePath: process.env.CHROME || "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
const context = await browser.newContext({
  viewport: { width: 1440, height: 900 },
  colorScheme: "light",
});
const page = await context.newPage();
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript(() => {
  window.subagentFrames = [];
  const Native = window.EventSource;
  window.EventSource = class extends Native {
    constructor(...args) {
      super(...args);
      this.addEventListener("space", (event) => {
        window.subagentFrames.push({ at: Date.now(), value: JSON.parse(event.data) });
        if (window.subagentFrames.length > 300) window.subagentFrames.shift();
      });
    }
  };
});
try {
  await page.goto(url);
  const member = page.getByRole("list", { name: "Members", exact: true }).getByRole("button").filter({ hasText: `@${alias}` });
  await member.waitFor({ timeout: 30_000 });
  const memberId = await member.locator("xpath=ancestor::li[1]").getAttribute("data-address-id");
  assert.equal(await member.locator(".subagent-count").count(), 0);
  await page.screenshot({ path: join(output, "before.png"), fullPage: true });
  await writeFile(join(output, "browser-ready"), "ready\n");
  const count = member.locator(".subagent-count");
  await count.waitFor({ timeout: 180_000 });
  await page.waitForFunction((name) => {
    const chip = [...document.querySelectorAll(".members .member")].find((node) => node.textContent.includes(`@${name}`));
    return /[23] subagents/.test(chip?.textContent || "");
  }, alias, { timeout: 120_000 });
  const observed = await count.innerText();
  console.log(`observed ${observed}`);
  await member.hover();
  const list = page.getByRole("list", { name: `Subagents of @${alias}`, exact: true });
  await list.waitFor({ timeout: 5_000 });
  assert.ok(await list.getByRole("listitem").count() >= 2);
  await page.screenshot({ path: join(output, "running-list-light.png"), fullPage: true });
  await member.click();
  const peek = page.locator(".peek-head .peek-subagents");
  await peek.waitFor({ timeout: 10_000 });
  assert.match(await peek.innerText(), /[23] subagents/);
  await page.screenshot({ path: join(output, "running-peek-light.png"), fullPage: true });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({ path: join(output, "running-peek-dark.png"), fullPage: true });
  await page.setViewportSize({ width: 1024, height: 700 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.screenshot({ path: join(output, "running-narrow-dark.png"), fullPage: true });
  await count.waitFor({ state: "detached", timeout: 240_000 });
  await peek.waitFor({ state: "detached", timeout: 10_000 });
  await member.waitFor({ state: "visible", timeout: 10_000 });
  assert.equal(await member.locator("xpath=ancestor::li[1]").getAttribute("data-address-id"), memberId);
  assert.equal(await page.locator(".peek-head .peek-alias").innerText(), `@${alias}`);
  const finalFrames = await page.evaluate(() => window.subagentFrames);
  const finalMember = finalFrames.at(-1)?.value.frame.participants.find((item) => item.address_id === memberId);
  assert.ok(finalMember, "the same member remains in the final frame");
  assert.deepEqual(finalMember.subagents, [], "the server reports an explicit empty running set");
  await page.screenshot({ path: join(output, "after-zero.png"), fullPage: true });
  assert.deepEqual(errors, []);
  await writeFile(join(output, "browser-result.json"), JSON.stringify({
    alias, observed, finalCount: 0, errors,
    frames: await page.evaluate(() => window.subagentFrames),
  }, null, 2));
  console.log("PASS count, hover list, Peek header, zero after completion, no page errors");
} catch (error) {
  await page.screenshot({ path: join(output, "failure.png"), fullPage: true }).catch(() => {});
  await writeFile(join(output, "failure-frames.json"), JSON.stringify(await page.evaluate(() => window.subagentFrames).catch(() => []), null, 2));
  throw error;
} finally {
  await browser.close();
}
