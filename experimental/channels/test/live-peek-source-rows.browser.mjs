import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, webkit } from "playwright-core";

const checkout = fileURLToPath(new URL("../../../", import.meta.url));
const profile = process.env.PEEK_SOURCE_PROFILE;
const output = process.env.PEEK_SOURCE_OUTPUT;
const needle = process.env.PEEK_SOURCE_TEXT ?? "Ask Codex to do anything";
assert.ok(
  profile,
  "Set PEEK_SOURCE_PROFILE to an isolated test-* profile directory",
);
assert.ok(output, "Set PEEK_SOURCE_OUTPUT to the screenshot directory");
const python = `
import json,os,signal,subprocess,sys
from pathlib import Path
sys.path.insert(0,str(Path(sys.argv[1])/'testbed/lib'))
directory=Path(sys.argv[2]).resolve()
assert directory.name.startswith('test-') and directory.parent.name=='profiles'
os.environ['TESTBED_HOME']=str(directory.parent.parent)
import testbed
p=testbed.Profile(directory.name);p.owned()
assert str(p.socket).startswith('/tmp/nd-tb-')
assert Path(p.env['HOME']).is_relative_to(directory)
assert Path(p.env['XDG_CONFIG_HOME']).is_relative_to(directory)
pane=p.state['panes']['codex']
operation=sys.argv[3]
if operation=='dashboard':
 print(p.namedrop('channels','--no-open','--json','--ui',str(Path(sys.argv[1])/'experimental/channels/dist')).stdout)
elif operation=='source':
 info=p.herdr('pane','get',pane)['pane']
 print(json.dumps({'rows':info['scroll']['viewport_rows'],'text':p.run(['herdr','pane','read',pane,'--source','visible']).stdout}))
elif operation=='control':
 info=p.herdr('pane','get',pane)['pane']
 child=subprocess.Popen(['herdr','terminal','session','control',info['terminal_id'],'--takeover','--rows',sys.argv[4],'--cols',sys.argv[5]],env=p.env,stdin=subprocess.PIPE,stdout=subprocess.PIPE)
 def stop(*_):
  child.terminate();child.wait(timeout=5);sys.exit(0)
 signal.signal(signal.SIGTERM,stop)
 for line in child.stdout:
  frame=json.loads(line)
  if frame.get('type')=='terminal.frame':
   print(json.dumps({'rows':frame['height'],'cols':frame['width']}),flush=True)
`;

function native(operation) {
  return JSON.parse(
    execFileSync("python3", ["-c", python, checkout, profile, operation], {
      encoding: "utf8",
    }),
  );
}

async function control(rows, cols) {
  const child = spawn(
    "python3",
    ["-c", python, checkout, profile, "control", String(rows), String(cols)],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) =>
      reject(new Error(`Controller exited ${code}`)),
    );
    child.stdout.once("data", resolve);
  });
  child.stdout.resume();
  let closed = false;
  return async () => {
    if (closed || child.exitCode !== null || child.signalCode !== null) return;
    closed = true;
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill();
    await exited;
  };
}

for (const [engine, type] of [
  ["chromium", chromium],
  ["webkit", webkit],
]) {
  for (const scale of [1, 2]) {
    test(`${engine} scale ${scale}: Live Peek retains the source bottom row and follows source resizing`, {
      timeout: 60000,
    }, async () => {
      await mkdir(output, { recursive: true });
      const source = native("source");
      assert.ok(
        source.rows > 40,
        "Use an idle test Codex pane taller than 40 rows",
      );
      assert.ok(
        source.text.includes(needle),
        "The native source contains the expected prompt",
      );
      const browser = await type.launch({
        headless: true,
        ...(engine === "chromium"
          ? {
              executablePath: process.env.CHROME ?? "/usr/bin/chromium",
              args: ["--no-sandbox"],
            }
          : {}),
      });
      const context = await browser.newContext({
        deviceScaleFactor: scale,
        viewport: { width: 1440, height: 900 },
      });
      const page = await context.newPage();
      let stop;
      const failures = [];
      try {
        await page.goto(native("dashboard").data.url);
        await page
          .locator(".member")
          .filter({ hasText: "@codex" })
          .first()
          .click();
        const rows = page.locator(".peek-buffer-visible .xterm-rows > div");
        await rows.first().waitFor();
        for (const [width, height] of [
          [1440, 900],
          [1728, 1117],
        ]) {
          await page.setViewportSize({ width, height });
          for (const expanded of [false, true]) {
            if (
              Boolean(
                await page
                  .getByRole("button", { name: "Restore", exact: true })
                  .count(),
              ) !== expanded
            ) {
              await page
                .getByRole("button", {
                  name: expanded ? "Expand" : "Restore",
                  exact: true,
                })
                .click();
            }
            await page.waitForTimeout(300);
            const text = await rows.allTextContents();
            const label = `${engine}-${scale}-${width}x${height}-${expanded ? "expanded" : "normal"}`;
            await page.screenshot({ path: join(output, `${label}.png`) });
            if (
              text.length !== source.rows ||
              !text.join("\n").includes(needle)
            ) {
              failures.push({
                label,
                sourceRows: source.rows,
                projectedRows: text.length,
                promptVisible: text.join("\n").includes(needle),
              });
            }
          }
        }
        await writeFile(
          join(output, `${engine}-${scale}.json`),
          JSON.stringify({ source, failures }, null, 2),
        );
        assert.deepEqual(
          failures,
          [],
          "The projection includes the source bottom rows and prompt",
        );
        stop = await control(source.rows + 20, 160);
        await page.waitForFunction(
          (expected) =>
            document.querySelectorAll(".peek-buffer-visible .xterm-rows > div")
              .length === expected,
          source.rows + 20,
        );
        await page.screenshot({
          path: join(output, `${engine}-${scale}-source-resized.png`),
        });
        assert.ok((await rows.allTextContents()).join("\n").includes(needle));
        await stop();
        stop = await control(source.rows, 160);
        await page.waitForFunction(
          (expected) =>
            document.querySelectorAll(".peek-buffer-visible .xterm-rows > div")
              .length === expected,
          source.rows,
        );
      } finally {
        if (stop) await stop();
        await context.close();
        await browser.close();
      }
    });
  }
}
