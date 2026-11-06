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
const alias = process.env.PEEK_SOURCE_ALIAS ?? "codex";
const columns = Number(process.env.PEEK_SOURCE_COLUMNS ?? 160);
const columnMarker = process.env.PEEK_SOURCE_COLUMN_MARKER;
assert.ok(
  profile,
  "Set PEEK_SOURCE_PROFILE to an isolated test-* profile directory",
);
assert.ok(output, "Set PEEK_SOURCE_OUTPUT to the screenshot directory");
const python = `
import json,os,signal,subprocess,sys,fcntl,termios,struct
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
pane=p.state['panes'][sys.argv[4]]
operation=sys.argv[3]
if operation=='dashboard':
 print(p.namedrop('channels','--no-open','--json','--ui',str(Path(sys.argv[1])/'experimental/channels/dist')).stdout)
elif operation=='source':
 info=p.herdr('pane','get',pane)['pane']
 process=p.herdr('pane','process-info','--pane',pane)['process_info']
 tty=subprocess.check_output(['/bin/ps','-p',str(process['shell_pid']),'-o','tty='],env=p.env,text=True).strip()
 fd=os.open('/dev/'+tty,os.O_RDONLY|os.O_NOCTTY|os.O_NONBLOCK)
 kernel_rows,kernel_cols,_,_=struct.unpack('HHHH',fcntl.ioctl(fd,termios.TIOCGWINSZ,b'\\0'*8));os.close(fd)
 print(json.dumps({'rows':info['scroll']['viewport_rows'],'cols':kernel_cols,'kernel_rows':kernel_rows,'text':p.run(['herdr','pane','read',pane,'--source','visible']).stdout}))
elif operation=='control':
 info=p.herdr('pane','get',pane)['pane']
 child=subprocess.Popen(['herdr','terminal','session','control',info['terminal_id'],'--takeover','--rows',sys.argv[5],'--cols',sys.argv[6]],env=p.env,stdin=subprocess.PIPE,stdout=subprocess.PIPE)
 def stop(*_):
  child.terminate()
  try: child.wait(timeout=2)
  except subprocess.TimeoutExpired:
   child.kill();child.wait(timeout=2)
  sys.exit(0)
 signal.signal(signal.SIGTERM,stop)
 for line in child.stdout:
  frame=json.loads(line)
  if frame.get('type')=='terminal.frame':
   print(json.dumps({'rows':frame['height'],'cols':frame['width']}),flush=True)
`;

function native(operation) {
  return JSON.parse(
    execFileSync(
      "python3",
      ["-c", python, checkout, profile, operation, alias],
      {
        encoding: "utf8",
      },
    ),
  );
}

async function control(t, rows, cols) {
  const child = spawn(
    "python3",
    [
      "-c",
      python,
      checkout,
      profile,
      "control",
      alias,
      String(rows),
      String(cols),
    ],
    { stdio: ["ignore", "pipe", "inherit"], detached: true },
  );
  let closed = false;
  const stop = async () => {
    if (closed || !child.pid) return;
    closed = true;
    const exited =
      child.exitCode !== null || child.signalCode !== null
        ? Promise.resolve()
        : new Promise((resolve) => child.once("exit", resolve));
    const signal = (name) => {
      try {
        process.kill(-child.pid, name);
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    };
    signal("SIGTERM");
    const kill = setTimeout(() => signal("SIGKILL"), 5000);
    try {
      await exited;
    } finally {
      signal("SIGKILL");
      clearTimeout(kill);
    }
  };
  t.after(stop);
  let timer;
  try {
    await new Promise((resolve, reject) => {
      timer = setTimeout(
        () => reject(new Error("Controller startup timed out")),
        5000,
      );
      child.once("error", reject);
      child.once("exit", (code) =>
        reject(new Error(`Controller exited ${code}`)),
      );
      child.stdout.once("data", resolve);
    });
    child.stdout.resume();
    return stop;
  } catch (error) {
    await stop();
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

for (const [engine, type] of [
  ["chromium", chromium],
  ["webkit", webkit],
]) {
  for (const scale of [1, 2]) {
    test(`${alias} ${columns}cols ${engine} scale ${scale}: Live Peek preserves source text and geometry through resizing`, {
      timeout: 60000,
    }, async (t) => {
      await mkdir(output, { recursive: true });
      let stop = await control(t, 80, columns);
      const source = native("source");
      assert.equal(source.rows, 80, "The test source has 80 rows");
      assert.equal(
        source.cols,
        columns,
        "The kernel source has the requested columns",
      );
      assert.ok(
        source.text.includes(needle),
        "The native source contains the expected text",
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
      t.after(() => browser.close());
      const context = await browser.newContext({
        deviceScaleFactor: scale,
        viewport: { width: 1440, height: 900 },
      });
      const page = await context.newPage();
      const failures = [];
      try {
        await page.goto(native("dashboard").data.url);
        await page
          .locator(".member")
          .filter({ hasText: `@${alias}` })
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
              !text.join("\n").includes(needle) ||
              (columnMarker &&
                text
                  .find((line) => line.includes(columnMarker))
                  ?.indexOf(columnMarker) !== 189)
            ) {
              failures.push({
                label,
                sourceRows: source.rows,
                projectedRows: text.length,
                promptVisible: text.join("\n").includes(needle),
                markerColumn: columnMarker
                  ? (text
                      .find((line) => line.includes(columnMarker))
                      ?.indexOf(columnMarker) ?? -1) + 1
                  : undefined,
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
        await stop();
        stop = await control(t, source.rows + 20, columns);
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
        stop = await control(t, source.rows, columns);
        await page.waitForFunction(
          (expected) =>
            document.querySelectorAll(".peek-buffer-visible .xterm-rows > div")
              .length === expected,
          source.rows,
        );
        assert.ok((await rows.allTextContents()).join("\n").includes(needle));
        if (columnMarker) {
          await stop();
          stop = await control(t, source.rows, columns + 20);
          await page.waitForFunction(
            (expected) =>
              [
                ...document.querySelectorAll(
                  ".peek-buffer-visible .xterm-rows > div",
                ),
              ].some((row) => row.textContent.length === expected),
            columns + 20,
          );
          assert.equal(
            (await rows.allTextContents())
              .find((line) => line.includes(columnMarker))
              ?.indexOf(columnMarker),
            189,
          );
          await page.screenshot({
            path: join(output, `${engine}-${scale}-columns-resized.png`),
          });
          await stop();
          stop = await control(t, source.rows, columns);
          await page.waitForFunction(
            (expected) =>
              [
                ...document.querySelectorAll(
                  ".peek-buffer-visible .xterm-rows > div",
                ),
              ].some((row) => row.textContent.length === expected),
            columns,
          );
          assert.equal(
            (await rows.allTextContents())
              .find((line) => line.includes(columnMarker))
              ?.indexOf(columnMarker),
            189,
          );
        }
      } finally {
        if (stop) await stop();
      }
    });
  }
}
