import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const [mintCommand, output, replier = "codex"] = process.argv.slice(2);
if (!mintCommand || !output)
  throw new Error(
    "usage: node shoot-ui3.mjs <mint-url-command> <output-dir> [replier alias]",
  );
mkdirSync(output, { recursive: true });
const mint = () =>
  execFileSync("sh", ["-c", mintCommand], { encoding: "utf8" }).trim();
const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
const log = { marks: [], errors: [], checks: {} };
const started = Date.now();
const mark = (name) =>
  log.marks.push({ name, at: (Date.now() - started) / 1000 });
const composer = (page) => page.locator("main").getByRole("combobox").first();
const threadComposer = (page) =>
  page.getByRole("complementary", { name: "Thread" }).getByRole("combobox");

try {
  const dir = join(output, ".video");
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    recordVideo: { dir, size: { width: 1440, height: 900 } },
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => log.errors.push(error.message));
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto(mint());
  await page.locator("[data-message-id]").first().waitFor();
  await page.waitForTimeout(1500);
  mark("loaded");
  const box = composer(page);
  await box.click();
  await page.keyboard.type("#al", { delay: 120 });
  await page.getByRole("listbox").waitFor();
  await page.waitForTimeout(700);
  await page.screenshot({ path: join(output, "popup-channel-1440-light.png") });
  await page.keyboard.press("Tab");
  await page.keyboard.type(`@${replier.slice(0, 2)}`, { delay: 120 });
  await page.getByRole("listbox").waitFor();
  await page.waitForTimeout(700);
  await page.screenshot({ path: join(output, "popup-member-1440-light.png") });
  await page.keyboard.press("Tab");
  log.checks.afterAutocomplete = await box.inputValue();
  await page.keyboard.type("please reply with the single word pong.", {
    delay: 40,
  });
  await page.waitForTimeout(500);
  mark("send");
  await page.keyboard.press("Enter");
  await page
    .getByText("Sending…")
    .first()
    .waitFor({ timeout: 5000 })
    .catch(() => {});
  await page.waitForTimeout(1200);
  await page.screenshot({ path: join(output, "pending-1440-light.png") });
  mark("wait-reply");
  const posted = page
    .locator("[data-message-id]")
    .filter({ hasText: "single word pong" })
    .last();
  await posted.locator(".reply-preview").waitFor({ timeout: 240000 });
  mark("reply-arrived");
  await page.waitForTimeout(1500);
  await page.screenshot({ path: join(output, "replied-1440-light.png") });
  await posted.locator(".reply-preview").click();
  await page.getByRole("complementary", { name: "Thread" }).waitFor();
  await page.waitForTimeout(1500);
  const reply = threadComposer(page);
  await reply.click();
  await page.keyboard.type("thanks, now reply with the single word ping.", {
    delay: 40,
  });
  await page.waitForTimeout(400);
  await page.screenshot({
    path: join(output, "thread-composer-1440-light.png"),
  });
  const pane = page.getByRole("complementary", { name: "Thread" });
  log.checks.toLabel = await pane
    .locator(".composer-to")
    .textContent()
    .catch(() => null);
  const byReplier = pane
    .locator("[data-message-id]")
    .filter({ has: page.locator(".author", { hasText: `@${replier}` }) });
  const repliesBefore = await byReplier.count();
  mark("thread-send");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(2000);
  log.checks.threadError = await pane
    .locator(".composer-error")
    .textContent()
    .catch(() => null);
  mark("wait-thread-reply");
  await page.waitForFunction(
    ([count]) => {
      const pane = document.querySelector('aside[aria-label="Thread"]');
      const rows = [...(pane?.querySelectorAll("[data-message-id]") ?? [])];
      return (
        rows.filter((row) =>
          row.querySelector(".author")?.textContent?.includes(count.alias),
        ).length > count.before
      );
    },
    [{ alias: `@${replier}`, before: repliesBefore }],
    { timeout: 240000 },
  );
  mark("thread-reply-arrived");
  await page.waitForTimeout(2500);
  await page.screenshot({
    path: join(output, "thread-replied-1440-light.png"),
  });
  log.checks.threadReplies = await page
    .getByRole("complementary", { name: "Thread" })
    .locator("[data-message-id]")
    .count();
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: join(output, "thread-replied-1440-dark.png") });
  log.perf = await page.evaluate(() =>
    Object.fromEntries(
      ["namedrop:keystroke-to-popup", "namedrop:enter-to-sent"].map((name) => [
        name,
        performance
          .getEntriesByName(name, "measure")
          .map((entry) => Math.round(entry.duration * 10) / 10),
      ]),
    ),
  );
  mark("end");
  await context.close();
  const [file] = readdirSync(dir);
  renameSync(join(dir, file), join(output, "walkthrough-raw.webm"));
  for (const [width, height] of [
    [1440, 900],
    [1024, 700],
  ])
    for (const theme of ["light", "dark"]) {
      const still = await browser.newContext({
        viewport: { width, height },
        reducedMotion: "reduce",
      });
      const view = await still.newPage();
      await view.emulateMedia({ colorScheme: theme });
      await view.goto(mint());
      await view.locator("[data-message-id]").first().waitFor();
      await view.waitForTimeout(800);
      const field = composer(view);
      await field.click();
      await view.keyboard.type("#al");
      await view.getByRole("listbox").waitFor();
      await view.screenshot({
        path: join(output, `popup-channel-${width}-${theme}.png`),
      });
      await view.keyboard.press("Tab");
      await view.keyboard.type("@");
      await view.getByRole("listbox").waitFor();
      await view.screenshot({
        path: join(output, `popup-member-${width}-${theme}.png`),
      });
      await view.keyboard.press("Escape");
      await field.fill("");
      await view.keyboard.type("hello without a recipient");
      await view.keyboard.press("Enter");
      await view.waitForTimeout(300);
      await view.screenshot({
        path: join(output, `guard-${width}-${theme}.png`),
      });
      await field.fill("");
      await view.keyboard.type("@ghost are you there?");
      await view.keyboard.press("Escape");
      await view.keyboard.press("Enter");
      await view
        .locator(".composer-error")
        .waitFor({ timeout: 8000 })
        .catch(() => {});
      await view.waitForTimeout(300);
      if (width === 1440)
        log.checks[`ghostError-${theme}`] = await view
          .locator(".composer-error")
          .textContent()
          .catch(() => null);
      await view.screenshot({
        path: join(output, `error-${width}-${theme}.png`),
      });
      await still.close();
    }
} finally {
  writeFileSync(join(output, "live-log.json"), JSON.stringify(log, null, 2));
  await browser.close();
}
console.log(JSON.stringify(log, null, 2));
