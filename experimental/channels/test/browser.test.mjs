import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname } from "node:path";
import { after, before, test } from "node:test";
import { chromium } from "playwright-core";

const key = (n) => n.toString(16).padStart(20, "0");
const fixture = JSON.parse(
  (
    await readFile(
      new URL("../src/fixtures/real-frames.jsonl", import.meta.url),
      "utf8",
    )
  )
    .trim()
    .split("\n")[0],
);
function frame(n, name, level = "calm") {
  const value = structuredClone(fixture);
  value.key = key(n);
  value.frame.space = {
    ...value.frame.space,
    key: key(n),
    workspace_id: `w${n}`,
    display_name: name,
    addressable_label: name,
  };
  value.frame.health = level === "needs_you" ? { level, items: 1 } : { level };
  return value;
}
const csp =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
};
let browser;
before(async () => {
  assert.ok(
    process.env.CHROME,
    "Set CHROME to a Chrome or Chromium executable",
  );
  browser = await chromium.launch({
    executablePath: process.env.CHROME,
    headless: true,
    args: ["--no-sandbox"],
  });
});
after(async () => {
  await browser?.close();
});

async function open(t, onConnect) {
  const clients = new Set();
  const connections = [];
  const errors = [];
  const server = createServer(async (req, res) => {
    if (req.url === "/api/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-store",
      });
      res.write("retry: 1000\n\n");
      const client = {
        send(name, data) {
          res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
        },
        destroy() {
          res.destroy();
        },
      };
      clients.add(client);
      connections.push(client);
      req.on("close", () => clients.delete(client));
      onConnect(client, connections.length);
      return;
    }
    try {
      const file = new URL(
        `../dist${req.url === "/" ? "/index.html" : req.url}`,
        import.meta.url,
      );
      const body = await readFile(file);
      res.writeHead(200, {
        "Content-Type":
          types[extname(file.pathname)] ?? "application/octet-stream",
        "Content-Security-Policy": csp,
      });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const page = await browser.newPage();
  page.setDefaultTimeout(5000);
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    window.cspErrors = [];
    document.addEventListener("securitypolicyviolation", (event) =>
      window.cspErrors.push(event.violatedDirective),
    );
  });
  t.after(async () => {
    try {
      assert.deepEqual(errors, []);
      assert.deepEqual(await page.evaluate(() => window.cspErrors), []);
    } finally {
      await page.close();
      for (const client of clients) client.destroy();
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const channels = page.getByRole("navigation", {
    name: "Channels",
    exact: true,
  });
  const row = (n) => channels.locator(`button[data-channel-key="${key(n)}"]`);
  const live = () => page.locator(".connection-dot.live").waitFor();
  return { page, channels, row, connections, live };
}
function replay(client, values, keys = values.map((value) => value.key)) {
  for (const value of values) client.send("space", value);
  client.send("ready", { keys });
}

test("fix2 item 3: disappearing focused Needs you shortcut restores canonical Channels focus", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [frame(1, "alpha"), frame(3, "gamma", "needs_you")]),
  );
  await ui.live();
  const shortcut = ui.page
    .getByRole("navigation", { name: "Needs you", exact: true })
    .getByRole("button");
  await shortcut.click();
  await shortcut.focus();
  assert.equal(
    await shortcut.evaluate((button) => button === document.activeElement),
    true,
  );
  ui.connections[0].send("space", frame(3, "gamma"));
  await shortcut.waitFor({ state: "detached" });
  await ui.page.waitForFunction(
    (selected) =>
      document.activeElement?.matches(
        `.channels-navigation button[data-channel-key="${selected}"]`,
      ),
    key(3),
  );
  assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
  await ui.page.keyboard.press("Home");
  assert.equal(
    await ui.row(1).evaluate((button) => button === document.activeElement),
    true,
  );
});

for (const mode of ["bad frame", "subset", "superset", "duplicates"]) {
  test(`fix2 items 1/2: ${mode} completes replay and later updates stay live`, async (t) => {
    const ui = await open(t, (client, n) => {
      if (n === 1) replay(client, [frame(1, "alpha"), frame(3, "gamma")]);
      else {
        const alpha = frame(1, "alpha");
        if (mode === "bad frame") alpha.frame.wire = 99;
        replay(
          client,
          [alpha, frame(3, "gamma")],
          mode === "subset"
            ? [key(3)]
            : mode === "superset"
              ? [key(1), key(3), key(4)]
              : mode === "duplicates"
                ? [key(1), key(1), key(3)]
                : [key(1), key(3)],
        );
      }
    });
    await ui.live();
    await ui.row(3).click();
    ui.connections[0].destroy();
    await ui.page.getByRole("status").waitFor();
    await ui.live();
    assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
    assert.equal(
      await ui.channels.getByRole("button").count(),
      mode === "subset" ? 1 : 2,
    );
    ui.connections.at(-1).send("space", frame(3, "gamma-live"));
    await ui.page
      .getByRole("heading", { name: "gamma-live", exact: true })
      .waitFor();
    assert.equal(ui.connections.length, 2);
  });
}

test("fix2 item 2: old server first load and reconnect fall back without pruning or extra streams", async (t) => {
  const ui = await open(t, (client, n) => {
    client.send("space", frame(3, "gamma"));
    if (n === 1) client.send("space", frame(1, "alpha"));
  });
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 2);
  await ui.row(3).click();
  ui.connections[0].send("space", frame(3, "gamma-live"));
  await ui.page
    .getByRole("heading", { name: "gamma-live", exact: true })
    .waitFor();
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 2);
  assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
  ui.connections[1].send("space", frame(3, "after-fallback"));
  await ui.page
    .getByRole("heading", { name: "after-fallback", exact: true })
    .waitFor();
  assert.equal(await ui.channels.getByRole("button").count(), 2);
  assert.equal(ui.connections.length, 2);
  assert.ok(
    (await ui.page.evaluate(
      () => performance.getEntriesByName("namedrop:frame-apply").length,
    )) >= 6,
  );
});

test("fix2 item 6: gone during replay removes both committed and pending channel", async (t) => {
  const ui = await open(t, (client, n) => {
    if (n === 1) replay(client, [frame(1, "alpha"), frame(3, "gamma")]);
    else {
      client.send("space", frame(1, "alpha"));
      client.send("gone", { key: key(1) });
      client.send("space", frame(3, "gamma"));
      client.send("ready", { keys: [key(1), key(3)] });
    }
  });
  await ui.live();
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 1);
  assert.equal(await ui.row(1).count(), 0);
  ui.connections[1].send("space", frame(1, "alpha"));
  await ui.row(1).waitFor();
});

test("fix3 item 2: slow first discovery stays loading at 2.4s until data arrives at 3s", async (t) => {
  const ui = await open(t, () => {});
  await new Promise((resolve) => setTimeout(resolve, 2400));
  assert.ok(
    (await ui.page.evaluate(
      () => performance.getEntriesByName("namedrop:frame-apply").length,
    )) > 0,
  );
  assert.equal(
    await ui.page.getByRole("heading", { level: 1 }).textContent(),
    "Connecting to Namedrop",
  );
  assert.equal(
    await ui.page.locator(".sidebar-footer").textContent(),
    "Connecting…Read only",
  );
  assert.equal(
    await ui.page.getByText("No live workspaces.", { exact: true }).count(),
    0,
  );
  await new Promise((resolve) => setTimeout(resolve, 600));
  ui.connections[0].send("space", frame(2, "beta"));
  ui.connections[0].send("space", frame(1, "alpha"));
  ui.connections[0].send("ready", { keys: [key(1), key(2)] });
  await ui.live();
  await ui.page.getByRole("heading", { name: "alpha", exact: true }).waitFor();
  assert.equal(await ui.row(1).getAttribute("aria-current"), "page");
  assert.equal(ui.connections.length, 1);
});

test("fix2 item 2: restarting cancels timeout and next open gets its own full interval", async (t) => {
  const ui = await open(t, (client) => client.send("space", frame(1, "alpha")));
  await ui.page.waitForFunction(() =>
    document
      .querySelector(".sidebar-footer")
      ?.textContent?.includes("Connecting"),
  );
  await new Promise((resolve) => setTimeout(resolve, 1100));
  ui.connections[0].send("restarting", {});
  await new Promise((resolve) => setTimeout(resolve, 1100));
  assert.equal(await ui.channels.getByRole("button").count(), 0);
  ui.connections[0].destroy();
  await ui.page.waitForFunction(() =>
    document
      .querySelector(".sidebar-footer")
      ?.textContent?.includes("Reconnecting"),
  );
  const deadline = Date.now() + 5000;
  while (ui.connections.length < 2 && Date.now() < deadline)
    await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(ui.connections.length, 2);
  await new Promise((resolve) => setTimeout(resolve, 1100));
  assert.equal(await ui.channels.getByRole("button").count(), 0);
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 1);
  assert.equal(ui.connections.length, 2);
});

test("fix2 item 2: frames do not postpone the per-open timeout", async (t) => {
  const ui = await open(t, () => {});
  await new Promise((resolve) => setTimeout(resolve, 1300));
  ui.connections[0].send("space", frame(1, "alpha"));
  await ui.page.locator(".connection-dot.live").waitFor({ timeout: 1400 });
  assert.equal(await ui.channels.getByRole("button").count(), 1);
});

test("fix2 item 2: error and new open invalidate the previous generation timeout", async (t) => {
  const ui = await open(t, (client) => client.send("space", frame(1, "alpha")));
  await new Promise((resolve) => setTimeout(resolve, 200));
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  const deadline = Date.now() + 5000;
  while (ui.connections.length < 2 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.equal(ui.connections.length, 2);
  await new Promise((resolve) => setTimeout(resolve, 1100));
  assert.equal(await ui.channels.getByRole("button").count(), 0);
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 1);
});

test("fix2 item 2: ready cancels the pending timeout measurement", async (t) => {
  const ui = await open(t, (client) => replay(client, [frame(1, "alpha")]));
  await ui.live();
  const count = await ui.page.evaluate(
    () => performance.getEntriesByName("namedrop:frame-apply").length,
  );
  await new Promise((resolve) => setTimeout(resolve, 2200));
  assert.equal(
    await ui.page.evaluate(
      () => performance.getEntriesByName("namedrop:frame-apply").length,
    ),
    count,
  );
});

function activity(n, name) {
  const value = frame(n, name);
  value.frame.messages.push({ message_id: "new-background-activity" });
  return value;
}
async function focused(ui, n) {
  await ui.page.waitForFunction(
    (selected) =>
      document.activeElement?.matches(
        `.channels-navigation button[data-channel-key="${selected}"]`,
      ),
    key(n),
  );
}

test("fix3 item 1: lateReady prunes omitted selected gamma and restores sorted read fallback focus", async (t) => {
  const ui = await open(t, (client, n) => {
    if (n === 1)
      replay(client, [frame(1, "zeta"), frame(2, "beta"), frame(3, "gamma")]);
  });
  await ui.live();
  await ui.row(3).click();
  ui.connections[0].send("space", activity(2, "beta"));
  await ui.row(2).getByRole("img", { name: "Unread activity" }).waitFor();
  await ui.row(3).focus();
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.live();
  assert.equal(ui.connections.length, 2);
  ui.connections[1].send("ready", { keys: [key(1), key(2)] });
  await ui.row(3).waitFor({ state: "detached" });
  assert.equal(await ui.row(2).getAttribute("aria-current"), "page");
  assert.equal(
    await ui.row(2).getByRole("img", { name: "Unread activity" }).count(),
    0,
  );
  await focused(ui, 2);
  await ui.page.keyboard.press("End");
  assert.equal(await ui.row(1).getAttribute("aria-current"), "page");
});

test("fix3 item 1: slowDiscoveryReconnect retains gamma selection, focus and alpha unread through late ready", async (t) => {
  const ui = await open(t, (client, n) => {
    if (n === 1)
      replay(client, [frame(1, "alpha"), frame(2, "beta"), frame(3, "gamma")]);
  });
  await ui.live();
  await ui.row(3).click();
  ui.connections[0].send("space", activity(1, "alpha"));
  await ui.row(1).getByRole("img", { name: "Unread activity" }).waitFor();
  await ui.row(3).focus();
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.live();
  ui.connections[1].send("space", frame(3, "gamma-current"));
  await ui.page
    .getByRole("heading", { name: "gamma-current", exact: true })
    .waitFor();
  const rejected = frame(1, "invalid alpha");
  rejected.frame.wire = 99;
  ui.connections[1].send("space", rejected);
  ui.connections[1].send("ready", { keys: [key(1), key(3), key(4)] });
  await ui.row(2).waitFor({ state: "detached" });
  assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
  assert.equal(
    await ui.row(1).getByRole("img", { name: "Unread activity" }).count(),
    1,
  );
  await focused(ui, 3);
  ui.connections[1].send("space", frame(2, "beta-live"));
  await ui.row(2).waitFor();
  ui.connections[1].send("ready", { keys: [] });
  ui.connections[1].send("space", frame(3, "gamma-after-duplicate"));
  await ui.page
    .getByRole("heading", { name: "gamma-after-duplicate", exact: true })
    .waitFor();
  assert.equal(await ui.channels.getByRole("button").count(), 3);
  assert.equal(ui.connections.length, 2);
});

test("fix3 item 1: tricklingReplay falls back on time then late ready chooses sidebar order", async (t) => {
  const ui = await open(t, () => {});
  await new Promise((resolve) => setTimeout(resolve, 1300));
  ui.connections[0].send("space", frame(2, "beta"));
  await ui.page.locator(".connection-dot.live").waitFor({ timeout: 1400 });
  assert.equal(await ui.row(2).getAttribute("aria-current"), "page");
  ui.connections[0].send("space", frame(1, "alpha"));
  ui.connections[0].send("ready", { keys: [key(1), key(2)] });
  await ui.page.getByRole("heading", { name: "alpha", exact: true }).waitFor();
  assert.equal(await ui.row(1).getAttribute("aria-current"), "page");
});

test("fix3 item 2: interrupted staged discovery and first retries remain loading until empty ready", async (t) => {
  const ui = await open(t, (client, n) => {
    if (n === 1) client.send("space", frame(1, "discarded"));
  });
  await ui.page.waitForFunction(
    () => performance.getEntriesByName("namedrop:frame-apply").length > 0,
  );
  await ui.page
    .getByRole("heading", { name: "Connecting to Namedrop", exact: true })
    .waitFor();
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.page
    .getByRole("heading", { name: "Connecting to Namedrop", exact: true })
    .waitFor();
  await new Promise((resolve) => setTimeout(resolve, 2400));
  assert.equal(
    await ui.page.getByRole("heading", { level: 1 }).textContent(),
    "Connecting to Namedrop",
  );
  assert.equal(await ui.channels.getByRole("button").count(), 0);
  assert.equal(
    await ui.page.locator(".sidebar-footer").textContent(),
    "Connecting…Read only",
  );
  ui.connections[1].send("ready", { keys: [] });
  await ui.page
    .getByRole("heading", { name: "No live workspaces", exact: true })
    .waitFor();
  await ui.live();
  assert.equal(ui.connections.length, 2);
});

test("fix3 item 2: legacy fallback data followed by last gone keeps known empty live view", async (t) => {
  const ui = await open(t, (client) => client.send("space", frame(1, "alpha")));
  await ui.live();
  await ui.row(1).waitFor();
  ui.connections[0].send("gone", { key: key(1) });
  await ui.page
    .getByRole("heading", { name: "No live workspaces", exact: true })
    .waitFor();
  assert.equal(
    await ui.page.locator(".sidebar-footer").textContent(),
    "Live updatesRead only",
  );
  assert.equal(ui.connections.length, 1);
});
