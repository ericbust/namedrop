import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const [mintCommand, output, channelName, prefix = ""] = process.argv.slice(2);
if (!mintCommand || !output || !channelName)
  throw new Error(
    "usage: node shoot-live.mjs <mint-url-command> <output-dir> <channel-name> [prefix]",
  );
mkdirSync(output, { recursive: true });

const url = execFileSync("sh", ["-c", mintCommand], {
  encoding: "utf8",
}).trim();
const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
try {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(url);
  const channels = page.getByRole("navigation", {
    name: "Channels",
    exact: true,
  });
  await channels.getByRole("button").first().waitFor();
  await channels
    .getByRole("button")
    .filter({ has: page.locator(".channel-name", { hasText: channelName }) })
    .first()
    .click();
  await page.waitForTimeout(600);
  const shot = (name) =>
    page.screenshot({
      path: join(output, `${prefix ? `${prefix}-` : ""}${name}.png`),
    });
  await page.emulateMedia({ colorScheme: "light" });
  await shot("channel-light");
  const thread = page.locator(".thread-button").last();
  if (await thread.count()) {
    await thread.click();
    await page
      .getByRole("complementary", { name: "Thread", exact: true })
      .waitFor();
    await page.waitForTimeout(300);
    await shot("thread-light");
    await page.keyboard.press("Escape");
  }
  await page.emulateMedia({ colorScheme: "dark" });
  await shot("channel-dark");
  await page.setViewportSize({ width: 1024, height: 700 });
  await shot("channel-1024-dark");
  await page.emulateMedia({ colorScheme: "light" });
  await shot("channel-1024-light");
  if (errors.length) console.error("page errors:", errors);
} finally {
  await browser.close();
}
