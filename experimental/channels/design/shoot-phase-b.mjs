import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, renameSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const [mintCommand, output, sendCommand] = process.argv.slice(2);
if (!mintCommand || !output)
  throw new Error(
    "usage: node shoot-phase-b.mjs <mint-url-command> <output-dir> [live-send-command]",
  );
mkdirSync(output, { recursive: true });
const mint = () =>
  execFileSync("sh", ["-c", mintCommand], { encoding: "utf8" }).trim();
const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
const errors = [];
const channel = (page, name) =>
  page
    .getByRole("navigation", { name: "Channels", exact: true })
    .getByRole("button")
    .filter({ has: page.locator(".row-name", { hasText: name }) })
    .first();

async function stills(width, height) {
  const context = await browser.newContext({
    viewport: { width, height },
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(mint());
  await page.locator("[data-message-id]").first().waitFor();
  await page.waitForTimeout(800);
  for (const theme of ["light", "dark"]) {
    await page.emulateMedia({ colorScheme: theme });
    const shot = (name) =>
      page.screenshot({ path: join(output, `${name}-${width}-${theme}.png`) });
    await channel(page, "testbed").click();
    await page.waitForTimeout(500);
    await shot("channel");
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > innerWidth,
    );
    if (overflow) errors.push(`horizontal overflow at ${width} ${theme}`);
    await page.locator(".reply-preview").last().click();
    await page.getByRole("complementary", { name: "Thread" }).waitFor();
    await page.waitForTimeout(400);
    await shot("thread");
    await page.keyboard.press("Escape");
    await channel(page, "beta").click();
    await page.waitForTimeout(500);
    await shot("beta");
    await channel(page, "alpha").click();
    await page.waitForTimeout(500);
    await shot("empty");
  }
  await context.close();
}

async function video() {
  const dir = join(output, ".video");
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    recordVideo: { dir, size: { width: 1440, height: 900 } },
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto(mint());
  await page.locator("[data-message-id]").first().waitFor();
  await page.waitForTimeout(4000);
  await page.locator(".reply-preview").nth(-2).click();
  await page.waitForTimeout(4000);
  await page.keyboard.press("Escape");
  await page.waitForTimeout(1000);
  await channel(page, "beta").click();
  await page.waitForTimeout(3500);
  await channel(page, "alpha").click();
  await page.waitForTimeout(2500);
  await channel(page, "testbed").click();
  await page.waitForTimeout(1500);
  if (sendCommand) {
    execFileSync("sh", ["-c", sendCommand]);
    await page.waitForTimeout(25000);
  }
  await page.emulateMedia({ colorScheme: "dark" });
  await page.waitForTimeout(4000);
  await page.locator(".reply-preview").last().click();
  await page.waitForTimeout(4000);
  await context.close();
  const [file] = readdirSync(dir);
  renameSync(join(dir, file), join(output, "walkthrough.webm"));
}

try {
  if (!process.env.VIDEO_ONLY) {
    await stills(1440, 900);
    await stills(1024, 700);
  }
  await video();
} finally {
  await browser.close();
}
if (errors.length) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
}
