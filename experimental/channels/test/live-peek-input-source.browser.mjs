import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, webkit } from "playwright-core";

const checkout = fileURLToPath(new URL("../../../", import.meta.url));
const profile = process.env.PEEK_INPUT_PROFILE;
const output = process.env.PEEK_INPUT_OUTPUT;
assert.ok(
  profile,
  "Set PEEK_INPUT_PROFILE to an owned test-* profile directory",
);
assert.ok(output, "Set PEEK_INPUT_OUTPUT to the evidence directory");
const python = `
import json,os,sqlite3,sys
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
operation,alias=sys.argv[3:5]
pane=p.state['panes'][alias]
if operation=='dashboard':
 print(p.namedrop('channels','--no-open','--json').stdout)
elif operation=='source':
 db=sqlite3.connect('file:'+str(directory/'state/herdr/plugins/namedrop/namedrop.sqlite3')+'?mode=ro',uri=True)
 db.row_factory=sqlite3.Row
 binding=dict(db.execute('SELECT binding_id,address_id,pane_id,bound_at_ms FROM pane_bindings WHERE pane_id=? AND active=1',(pane,)).fetchone())
 observation=db.execute("SELECT agent_session_id,observed_at_ms FROM agent_session_observations WHERE address_id=? AND status='idle' AND observed_at_ms>=? ORDER BY observed_at_ms DESC,rowid DESC LIMIT 1",(binding['address_id'],binding['bound_at_ms'])).fetchone()
 native=next(row for row in p.herdr('pane','list')['panes'] if row['pane_id']==pane)
 print(json.dumps({'binding':binding,'idle':dict(observation) if observation else None,'terminal':native['terminal_id'],'session':native.get('agent_session'),'text':p.run(['herdr','pane','read',pane,'--source','visible']).stdout}))
`;

function native(operation, alias) {
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

for (const [engine, type] of [
  ["chromium", chromium],
  ["webkit", webkit],
]) {
  for (const alias of ["codex", "claude"]) {
    test(`${engine} ${alias}: Type to agent keeps the same Live Peek through the reply`, {
      timeout: 60000,
    }, async (t) => {
      await mkdir(output, { recursive: true });
      const label = `${engine}-${alias}`;
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
      const page = await browser.newPage({
        viewport: { width: 1728, height: 1117 },
        deviceScaleFactor: 2,
      });
      const posts = [];
      page.on("response", (response) => {
        if (
          response.request().method() === "POST" &&
          response.url().endsWith("/input")
        )
          posts.push(response.status());
      });
      await page.goto(native("dashboard", alias).data.url);
      await page
        .locator(".member")
        .filter({ hasText: `@${alias}` })
        .first()
        .click();
      const rows = page.locator(".peek-buffer-visible .xterm-rows > div");
      await rows.first().waitFor();
      const before = native("source", alias);
      assert.ok(before.idle, "The real agent has completed its seed turn");
      await page.screenshot({ path: join(output, `${label}-before.png`) });
      const marker = `PEEKREPLY${Date.now()}${alias.toUpperCase()}`;
      await page
        .getByRole("button", { name: "Type to agent", exact: true })
        .click();
      const field = page.getByRole("combobox", {
        name: `Message to @${alias}`,
      });
      await field.fill(`Reply exactly ${marker}. Do not use tools.`);
      await field.press("Enter");
      if (alias === "codex") {
        await page.waitForTimeout(200);
        await page
          .getByRole("button", { name: "Send Enter", exact: true })
          .click();
      }
      let after;
      const deadline = Date.now() + 30000;
      do {
        await page.waitForTimeout(300);
        after = native("source", alias);
      } while (
        Date.now() < deadline &&
        (!after.text
          .split("\n")
          .some(
            (line) => line.includes(marker) && !line.includes("Reply exactly"),
          ) ||
          after.idle?.observed_at_ms <= before.idle.observed_at_ms)
      );
      await page.waitForTimeout(1500);
      const text = (await page.locator(".peek").textContent()) ?? "";
      await page.screenshot({ path: join(output, `${label}-after.png`) });
      await writeFile(
        join(output, `${label}.json`),
        JSON.stringify({ marker, posts, before, after, text }, null, 2),
      );
      assert.ok(
        after.text
          .split("\n")
          .some(
            (line) => line.includes(marker) && !line.includes("Reply exactly"),
          ),
        "The real agent replied to the browser input",
      );
      assert.ok(after.idle.observed_at_ms > before.idle.observed_at_ms);
      assert.deepEqual(after.binding, before.binding);
      assert.equal(after.terminal, before.terminal);
      assert.deepEqual(after.session, before.session);
      assert.ok(posts.length > 0 && posts.every((status) => status === 204));
      assert.equal(text.includes("This pane changed"), false);
      assert.ok(
        (await rows.allTextContents()).some(
          (line) => line.includes(marker) && !line.includes("Reply exactly"),
        ),
        "The reply is visible in the same Live Peek without reopening",
      );
    });
  }
}
