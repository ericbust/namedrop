import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { chromium } from "playwright-core";

const here = new URL(".", import.meta.url).pathname;
const output = process.argv[2] ?? join(here, "after");
mkdirSync(output, { recursive: true });
const page_ = pathToFileURL(join(here, "mockup/index.html")).href;
const shots = [
  ["channel", "light"],
  ["channel", "dark"],
  ["unanswered", "light"],
  ["unanswered", "dark"],
  ["calm", "light"],
  ["thread", "light"],
  ["thread", "dark"],
  ["thread-terminal", "light"],
  ["empty", "light"],
  ["empty", "dark"],
  ["reconnect", "light"],
  ["pile", "light"],
];
const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
try {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  for (const [view, theme] of shots) {
    await page.goto(`${page_}?view=${view}&theme=${theme}&shot=1`);
    await page.waitForTimeout(150);
    await page.screenshot({ path: join(output, `${view}-${theme}.png`) });
  }
  await page.setViewportSize({ width: 1024, height: 700 });
  for (const theme of ["light", "dark"]) {
    await page.goto(`${page_}?view=thread&theme=${theme}&shot=1`);
    await page.waitForTimeout(150);
    await page.screenshot({
      path: join(output, `thread-1024-${theme}.png`),
    });
  }
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > innerWidth,
  );
  if (overflow) console.error("horizontal overflow at 1024");
  if (errors.length) console.error("page errors:", errors);
} finally {
  await browser.close();
}
