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
function message(message_id, text = message_id, extra = {}) {
  return {
    message_id,
    thread_id: message_id,
    in_reply_to: null,
    at_ms: 100,
    author: "human",
    kind: "mention",
    body: { text, truncated: false },
    legs: [],
    ...extra,
  };
}
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

async function open(t, onConnect, history = () => [], { path = "/" } = {}) {
  const clients = new Set();
  const connections = [];
  const errors = [];
  const historyRequests = [];
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
    const url = new URL(req.url ?? "/", "http://localhost");
    if (/^\/w\/[^/]+\/messages$/.test(url.pathname)) {
      historyRequests.push(url);
      try {
        const rows = await history(url);
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify(rows));
      } catch {
        res.writeHead(404).end();
      }
      return;
    }
    try {
      const page = url.pathname === "/" || /^\/w\/[^/]+$/.test(url.pathname);
      const file = new URL(
        `../dist${page ? "/index.html" : url.pathname}`,
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
    window.initialHistoryLength = history.length;
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
  await page.goto(`http://127.0.0.1:${server.address().port}${path}`);
  const channels = page.getByRole("navigation", {
    name: "Channels",
    exact: true,
  });
  const row = (n) => channels.locator(`button[data-channel-key="${key(n)}"]`);
  const live = () => page.locator(".connection-dot.live").waitFor();
  return { page, channels, row, connections, live, historyRequests };
}
function replay(client, values, keys = values.map((value) => value.key)) {
  for (const value of values) client.send("space", value);
  client.send("ready", { keys });
}

function attentionFrame(source) {
  const value = frame(1, "alpha", "needs_you");
  value.frame.participants[0].activity = "blocked";
  value.frame.attention = [
    {
      source_kind: source,
      summary: { text: "Waiting on approval or a question", truncated: false },
      opened_at_ms: 42,
    },
  ];
  return value;
}

test("fix5: the page declares a data favicon", async (t) => {
  const ui = await open(t, (client) => replay(client, [frame(1, "alpha")]));
  await ui.live();
  assert.equal(
    await ui.page.locator('link[rel="icon"]').getAttribute("href"),
    "data:,",
  );
});

test("fix5: blocked agent_status attention survives replay and future attention updates stay live", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [attentionFrame("agent_status")]),
  );
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 1);
  await ui.page
    .getByText("Waiting on approval or a question", { exact: true })
    .waitFor();
  assert.equal(
    await ui.page.locator('.member-row[data-activity="blocked"]').count(),
    1,
  );
  assert.equal(
    await ui.row(1).getByRole("img", { name: "Needs you" }).count(),
    1,
  );
  const update = attentionFrame("future_attention");
  update.frame.space.display_name = "alpha updated";
  update.frame.attention[0].summary = {
    text: "New future request",
    truncated: true,
  };
  ui.connections[0].send("space", update);
  await ui.page
    .getByRole("heading", { name: "alpha updated", exact: true })
    .waitFor();
  await ui.page.getByText("New future request…", { exact: true }).waitFor();
  assert.equal(
    await ui.page
      .getByText("Waiting on approval or a question", { exact: true })
      .count(),
    0,
  );
  ui.connections[0].send("space", frame(1, "alpha resolved"));
  await ui.page
    .getByRole("heading", { name: "alpha resolved", exact: true })
    .waitFor();
  assert.equal(
    await ui.page.getByRole("region", { name: "Attention needed" }).count(),
    0,
  );
  assert.equal(ui.connections.length, 1);
});

test("fix5: future statuses stay neutral and duplicate generic attention rows survive live replacement", async (t) => {
  const initial = frame(1, "alpha");
  const member = initial.frame.participants[0];
  initial.frame.participants = [
    {
      ...member,
      activity: "future_activity",
      kind: { type: "future_kind" },
      role: null,
      pane_label: null,
    },
  ];
  const ui = await open(t, (client) => replay(client, [initial]));
  const keyErrors = [];
  ui.page.on("console", (message) => {
    if (
      message.type() === "error" &&
      /same key|unique.*key/i.test(message.text())
    )
      keyErrors.push(message.text());
  });
  await ui.live();
  await ui.page.getByText("Unknown status", { exact: true }).waitFor();
  assert.equal(
    await ui.page.locator(".member-description").textContent(),
    "Member",
  );
  const color = await ui.page
    .locator('.presence-dot[data-activity="unknown"]')
    .evaluate((dot) => getComputedStyle(dot).backgroundColor);
  const offline = frame(2, "beta");
  offline.frame.participants[0].activity = "offline";
  ui.connections[0].send("space", offline);
  await ui.row(2).click();
  assert.equal(
    color,
    await ui.page
      .locator('.presence-dot[data-activity="offline"]')
      .evaluate((dot) => getComputedStyle(dot).backgroundColor),
  );
  await ui.row(1).click();
  const update = attentionFrame("future_one");
  update.frame.attention.push({
    ...structuredClone(update.frame.attention[0]),
    source_kind: "future_two",
  });
  ui.connections[0].send("space", update);
  const rows = ui.page
    .getByRole("region", { name: "Attention needed" })
    .getByRole("listitem");
  await rows.nth(1).waitFor();
  assert.equal(await rows.count(), 2);
  update.frame.attention[0].summary.text = "First request changed";
  ui.connections[0].send("space", update);
  await rows.getByText("First request changed", { exact: true }).waitFor();
  assert.equal(await rows.count(), 2);
  assert.equal(
    await rows.nth(1).textContent(),
    "Waiting on approval or a question",
  );
  for (const level of ["future_health", "paused", "inactive"]) {
    update.frame.health = { level };
    ui.connections[0].send("space", update);
    await ui.page
      .getByText(
        level === "future_health"
          ? "Status unavailable"
          : level === "paused"
            ? "Paused"
            : "Not active",
        { exact: false },
      )
      .waitFor();
    assert.equal(
      await ui.page.getByRole("region", { name: "Attention needed" }).count(),
      0,
    );
    assert.equal(
      await ui.row(1).getByRole("img", { name: "Needs you" }).count(),
      0,
    );
  }
  assert.deepEqual(keyErrors, []);
});

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
  value.frame.messages.push(message("new-background-activity"));
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

test("fix3 item 1: lateReady prunes omitted selected gamma and restores sorted fallback focus, read after dwell", async (t) => {
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
  await ui
    .row(2)
    .getByRole("img", { name: "Unread activity" })
    .waitFor({ state: "detached" });
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

function exchange() {
  const value = frame(1, "alpha");
  value.frame.messages = [
    message("parent", "@codex please check this", {
      thread_id: "exchange",
      legs: [{ to: "codex", stage: { stage: "working", acked_at_ms: 100 } }],
    }),
    message("reply", "Checked and ready", {
      thread_id: "exchange",
      in_reply_to: "parent",
      kind: "reply",
    }),
  ];
  value.frame.threads = [
    {
      thread_id: "exchange",
      title: { text: "Check", truncated: false },
      message_count: 2,
    },
  ];
  return value;
}
test("UI-2: parent opens a live thread, Escape returns focus, and channels remain isolated", async (t) => {
  const initial = exchange();
  const beta = frame(2, "beta");
  beta.frame.messages = [message("beta-only", "Only beta")];
  const ui = await open(t, (client) => replay(client, [initial, beta]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Channel messages" });
  assert.equal(await timeline.locator("[data-message-id]").count(), 1);
  const button = timeline.getByRole("button", { name: "1 reply", exact: true });
  await button.click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  await pane.getByText("Checked and ready", { exact: true }).waitFor();
  const update = structuredClone(initial);
  update.frame.messages.push(
    message("reply-2", "Live second reply", {
      thread_id: "exchange",
      in_reply_to: "parent",
      kind: "reply",
    }),
  );
  update.frame.threads[0].message_count = 3;
  ui.connections[0].send("space", update);
  await pane.getByText("Live second reply", { exact: true }).waitFor();
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  assert.equal(
    await timeline
      .getByRole("button", { name: "2 replies", exact: true })
      .evaluate((element) => element === document.activeElement),
    true,
  );
  await ui.row(2).click();
  await timeline.getByText("Only beta", { exact: true }).waitFor();
  assert.equal(await timeline.locator('[data-message-id="parent"]').count(), 0);
  assert.equal(await pane.count(), 0);
  await ui.row(1).click();
  await timeline.locator('[data-message-id="parent"]').waitFor();
  await ui.page.waitForFunction(
    () =>
      performance.getEntriesByName("namedrop:frame-to-message-paint").length >
      0,
  );
});

test("UI-2: code, safe links, report cards, long bodies and unknown variants render as text under CSP", async (t) => {
  const value = frame(1, "alpha");
  const hostile =
    '<img src=x onerror=alert(1)> <script>alert("bad")</script> javascript:alert(1)';
  value.frame.messages = [
    message("hostile", hostile),
    message(
      "format",
      'Example:\n```js\nconst value = "<safe>";\n```\nRead https://example.com/docs.',
    ),
    message("long", `${"Long body ".repeat(90)}END OF BODY`, {
      body: { text: `${"Long body ".repeat(90)}END OF BODY`, truncated: true },
    }),
    message("future", "Future messages survive", {
      kind: "future_kind",
      legs: [{ to: "agent", stage: { stage: "future_stage" } }],
    }),
    message("report", "Two tasks completed", { kind: "result_set" }),
    message("nudge", "Please continue", { kind: "nudge" }),
    message("hook", "Hook received", { kind: "hook" }),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  let dialogs = 0;
  ui.page.on("dialog", async (dialog) => {
    dialogs++;
    await dialog.dismiss();
  });
  await ui.live();
  await ui.page.getByText(hostile, { exact: true }).waitFor();
  assert.equal(
    await ui.page.locator(".message img, .message script").count(),
    0,
  );
  assert.equal(dialogs, 0);
  const code = ui.page.locator(".message-body pre code");
  assert.equal(await code.textContent(), 'const value = "<safe>";\n');
  assert.match(
    await code.evaluate((element) => getComputedStyle(element).fontFamily),
    /monospace/,
  );
  const link = ui.page.getByRole("link", {
    name: "https://example.com/docs",
    exact: true,
  });
  assert.equal(await link.getAttribute("href"), "https://example.com/docs");
  assert.equal(await link.getAttribute("rel"), "noreferrer");
  assert.equal(
    await ui.page.locator('.message a[href^="javascript:"]').count(),
    0,
  );
  const long = ui.page.locator('[data-message-id="long"]');
  assert.equal(
    (await long.locator(".message-body").textContent()).includes("END OF BODY"),
    false,
  );
  await long.getByRole("button", { name: "Show more", exact: true }).click();
  assert.equal(
    (await long.locator(".message-body").textContent()).includes("END OF BODY"),
    true,
  );
  await long
    .getByText(
      "Message shortened by the server. Only the available text is shown.",
      { exact: true },
    )
    .waitFor();
  await ui.page.getByText("Future messages survive", { exact: true }).waitFor();
  await ui.page.getByText("Status unavailable", { exact: false }).waitFor();
  assert.equal(
    await ui.page.getByRole("heading", { name: "Coordination report" }).count(),
    1,
  );
  await ui.page.getByText("Please continue", { exact: true }).waitFor();
  await ui.page.getByText("Hook received", { exact: true }).waitFor();
});

test("UI-2: scrolling to top loads history without shifting the first visible message, including live updates and switching", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = Array.from({ length: 100 }, (_, index) =>
    message(
      `m${index + 101}`,
      `Message ${index + 101}\nA second line of conversation.`,
    ),
  );
  const beta = frame(2, "beta");
  beta.frame.messages = [message("beta-only", "Beta conversation")];
  const ui = await open(
    t,
    (client) => replay(client, [value, beta]),
    async (url) => {
      assert.equal(url.pathname, `/w/${key(1)}/messages`);
      assert.equal(url.searchParams.get("limit"), "50");
      if (url.searchParams.get("before") !== "m101") return [];
      await new Promise((resolve) => setTimeout(resolve, 120));
      return Array.from({ length: 50 }, (_, index) =>
        message(
          `m${100 - index}`,
          `Message ${100 - index}\nEarlier conversation.`,
        ),
      );
    },
  );
  await ui.live();
  const scroll = ui.page.locator(".timeline-scroll");
  const first = ui.page.locator('[data-message-id="m101"]');
  await scroll.evaluate((element) => {
    element.scrollTop = 0;
  });
  await ui.page.getByText("Loading older messages…", { exact: true }).waitFor();
  const before = await first.evaluate(
    (element) => element.getBoundingClientRect().top,
  );
  const update = structuredClone(value);
  update.frame.messages.push(message("m201", "A live arrival"));
  ui.connections[0].send("space", update);
  await ui.page.locator('[data-message-id="m51"]').waitFor();
  const after = await first.evaluate(
    (element) => element.getBoundingClientRect().top,
  );
  assert.ok(
    Math.abs(after - before) <= 2,
    `anchor shifted ${after - before}px`,
  );
  assert.equal(
    await ui.page.locator(".timeline [data-message-id]").count(),
    151,
  );
  assert.equal(ui.historyRequests.length, 1);
  await ui.row(2).click();
  await ui.page.getByText("Beta conversation", { exact: true }).waitFor();
  assert.equal(await ui.page.locator('[data-message-id="m101"]').count(), 0);
  await ui.row(1).click();
  await first.waitFor();
  assert.ok(
    Math.abs(
      (await first.evaluate((element) => element.getBoundingClientRect().top)) -
        after,
    ) <= 2,
  );
});

test("UI-2: reconnect gaps repair through history and receipt-only live updates repaint", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [message("a"), message("b")];
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    (url) => {
      assert.equal(url.searchParams.get("before"), "f");
      return [message("e"), message("d"), message("c"), message("b")];
    },
  );
  await ui.live();
  const live = structuredClone(value);
  live.frame.messages = [
    message("f", "Please wait", {
      legs: [{ to: "agent", stage: { stage: "queued" } }],
    }),
    message("g"),
  ];
  ui.connections[0].send("space", live);
  await ui.page.locator('[data-message-id="c"]').waitFor();
  assert.deepEqual(
    await ui.page
      .locator(".timeline [data-message-id]")
      .evaluateAll((rows) => rows.map((row) => row.dataset.messageId)),
    ["a", "b", "c", "d", "e", "f", "g"],
  );
  assert.equal(await ui.page.locator(".message-gap").count(), 0);
  live.frame.messages[0].legs[0].stage = { stage: "held" };
  ui.connections[0].send("space", live);
  await ui.page.locator('[data-message-id="f"] [data-stage="held"]').waitFor();
});

test("UI-2: paint entries wait for replay publication and do not repeat after unrelated renders", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [message("a")];
  const ui = await open(t, (client) => client.send("space", value));
  await ui.page.waitForFunction(
    () => performance.getEntriesByName("namedrop:frame-apply").length > 0,
  );
  assert.equal(
    await ui.page.evaluate(
      () =>
        performance.getEntriesByName("namedrop:frame-to-message-paint").length,
    ),
    0,
  );
  ui.connections[0].send("ready", { keys: [key(1)] });
  await ui.live();
  await ui.page.waitForFunction(
    () =>
      performance.getEntriesByName("namedrop:frame-to-message-paint").length ===
      1,
  );
  const beta = frame(2, "beta");
  ui.connections[0].send("space", beta);
  await ui.row(2).waitFor();
  await ui.page.waitForFunction(
    () =>
      performance.getEntriesByName("namedrop:frame-to-message-paint").length ===
      1,
  );
  await ui.row(2).click();
  await ui.page.waitForTimeout(80);
  assert.equal(
    await ui.page.evaluate(
      () =>
        performance.getEntriesByName("namedrop:frame-to-message-paint").length,
    ),
    1,
  );
  for (let n = 0; n < 210; n++) {
    const update = structuredClone(beta);
    update.frame.messages = [message("batch", `Update ${n}`)];
    ui.connections[0].send("space", update);
  }
  await ui.page.getByText("Update 209", { exact: true }).waitFor();
  await ui.page.waitForTimeout(80);
  const count = await ui.page.evaluate(
    () =>
      performance.getEntriesByName("namedrop:frame-to-message-paint").length,
  );
  assert.ok(count > 0 && count <= 200, `unbounded paint measures: ${count}`);
});

test("UI-2: paint samples continue during an ongoing selected-channel stream", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [message("a", "Initial message")];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  await ui.page.waitForFunction(
    () =>
      performance.getEntriesByName("namedrop:frame-to-message-paint").length >
      0,
  );
  await ui.page.evaluate(() =>
    performance.clearMeasures("namedrop:frame-to-message-paint"),
  );
  let tick = 0;
  const updates = setInterval(() => {
    value.frame.messages[0].body.text = `Streaming message ${++tick}`;
    ui.connections[0].send("space", value);
  }, 4);
  try {
    await ui.page.waitForFunction(
      () =>
        performance.getEntriesByName("namedrop:frame-to-message-paint").length >
        0,
      undefined,
      { timeout: 1500 },
    );
    assert.ok(
      tick > 1,
      "the stream is still updating when a paint sample arrives",
    );
  } finally {
    clearInterval(updates);
  }
});

test("UI-2: message history is a named keyboard-scrollable region", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = Array.from({ length: 150 }, (_, n) =>
    message(`m${n}`, `Message ${n}`),
  );
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const history = ui.page.getByRole("region", {
    name: "Message history",
    exact: true,
  });
  await history.focus();
  const before = await history.evaluate((element) => element.scrollTop);
  assert.ok(before > 0);
  await ui.page.keyboard.press("PageUp");
  await ui.page.waitForFunction(
    (top) => document.querySelector(".timeline-scroll").scrollTop < top,
    before,
  );
});

test("UI-2 fix1: every delivery stage shows its visible receipt label", async (t) => {
  const value = frame(1, "alpha");
  const stages = [
    [{ stage: "queued" }, "Queued"],
    [{ stage: "delivered" }, "Delivered"],
    [{ stage: "working", acked_at_ms: 100 }, "Working (acknowledged)"],
    [{ stage: "replied", reply_id: "reply" }, "Replied"],
    [{ stage: "completed", kind: "success" }, "Completed · success"],
    [{ stage: "held" }, "Held"],
    [{ stage: "failed" }, "Failed"],
    [{ stage: "cancelled" }, "Cancelled"],
    [{ stage: "future_stage" }, "Status unavailable"],
  ];
  value.frame.messages = stages.map(([stage], n) =>
    message(`stage${n}`, `Delivery ${n}`, { legs: [{ to: "agent", stage }] }),
  );
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  for (const [index, [, label]] of stages.entries()) {
    const receipt = ui.page.locator(
      `[data-message-id="stage${index}"] .receipts li`,
    );
    await receipt.waitFor();
    assert.equal(await receipt.textContent(), `agent · ${label}`);
  }
});

test("UI-2 fix1: collapsed previews preserve full href and Unicode boundaries", async (t) => {
  const href = `https://example.com/${"y".repeat(300)}`;
  const value = frame(1, "alpha");
  value.frame.messages = [
    message("cut-url", `token:${"x".repeat(400)} and ${href}`),
    message("unicode", `${"x".repeat(479)}😀 trailing text`),
    message(
      "fenced",
      `${"x".repeat(450)}\n\x60\x60\x60js\nhttps://example.com/${"z".repeat(100)}\n\x60\x60\x60`,
    ),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const row = ui.page.locator('[data-message-id="cut-url"]');
  const link = row.getByRole("link");
  assert.equal(await link.getAttribute("href"), href);
  assert.ok((await link.textContent()).length < href.length);
  assert.equal((await link.textContent()).includes("…"), false);
  assert.equal(
    (await row.locator(".message-body").textContent()).endsWith("…"),
    true,
  );
  const unicode = ui.page.locator('[data-message-id="unicode"]');
  assert.equal(
    await unicode.locator(".message-body").textContent(),
    `${"x".repeat(479)}…`,
  );
  await unicode.getByRole("button", { name: "Show more", exact: true }).click();
  assert.equal(
    await unicode.locator(".message-body").textContent(),
    `${"x".repeat(479)}😀 trailing text`,
  );
  assert.equal(
    await ui.page.locator('[data-message-id="fenced"] a').count(),
    0,
  );
  assert.equal(
    await ui.page.locator('[data-message-id="fenced"] pre').count(),
    1,
  );
});

test("UI-2 fix1: switching closes the thread, live frames keep sidebar focus, and Escape stays scoped", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [exchange(), frame(2, "beta")]),
  );
  await ui.live();
  const button = ui.page
    .locator(".timeline")
    .getByRole("button", { name: "1 reply", exact: true });
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  assert.equal(await button.getAttribute("aria-expanded"), "false");
  const paneId = await button.getAttribute("aria-controls");
  assert.ok(paneId);
  await button.click();
  await pane.waitFor();
  assert.equal(await pane.getAttribute("id"), paneId);
  assert.equal(await button.getAttribute("aria-expanded"), "true");
  assert.equal(
    await pane
      .getByRole("button", { name: "Close thread" })
      .evaluate((el) => el === document.activeElement),
    true,
  );
  await button.click();
  assert.equal(
    await pane
      .getByRole("button", { name: "Close thread" })
      .evaluate((el) => el === document.activeElement),
    true,
  );
  await ui.row(2).click();
  await pane.waitFor({ state: "detached" });
  await ui.row(1).click();
  await focused(ui, 1);
  assert.equal(await pane.count(), 0);
  assert.equal(await button.getAttribute("aria-controls"), paneId);
  assert.equal(await button.getAttribute("aria-expanded"), "false");
  await button.click();
  await pane.waitFor();
  await ui.row(1).focus();
  ui.connections[0].send("space", exchange());
  await ui.row(1).click();
  await focused(ui, 1);
  assert.equal(await pane.count(), 1);
  assert.equal(await button.getAttribute("aria-expanded"), "true");
  await ui.page.keyboard.press("Escape");
  assert.equal(await pane.count(), 1);
  await ui.page.evaluate(() => {
    const form = document.createElement("form");
    form.innerHTML = '<input aria-label="Other input">';
    document.body.append(form);
    form.querySelector("input").focus();
  });
  await ui.page.keyboard.press("Escape");
  assert.equal(await pane.count(), 1);
  await ui.page.evaluate(() => {
    const dialog = document.createElement("dialog");
    dialog.innerHTML = "<button>Dialog action</button>";
    document.body.append(dialog);
    dialog.showModal();
  });
  await ui.page.keyboard.press("Escape");
  assert.equal(await pane.count(), 1);
  await pane.getByRole("button", { name: "Close thread" }).focus();
  await ui.page.evaluate(() => {
    document.activeElement.addEventListener(
      "keydown",
      (event) => event.preventDefault(),
      { once: true },
    );
  });
  await ui.page.keyboard.press("Escape");
  assert.equal(await pane.count(), 1);
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  assert.equal(await button.getAttribute("aria-expanded"), "false");
  assert.equal(
    await button.evaluate((el) => el === document.activeElement),
    true,
  );
  await button.click();
  await ui.page.evaluate(() => document.activeElement.blur());
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
});

test("UI-2 fix1: capped history preserves anchor and jumps to refreshed latest without bridge loops", async (t) => {
  const value = frame(1, "alpha");
  const rows = (from, to) =>
    Array.from({ length: to - from + 1 }, (_, n) =>
      message(`m${from + n}`, `Message ${from + n}`),
    );
  value.frame.messages = rows(1001, 1150);
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    async (url) => {
      const before = Number(url.searchParams.get("before").slice(1));
      await new Promise((resolve) => setTimeout(resolve, 60));
      return rows(Math.max(1, before - 50), before - 1).reverse();
    },
  );
  await ui.live();
  const scroll = ui.page.locator(".timeline-scroll");
  const count = () => ui.page.locator(".timeline [data-message-id]").count();
  for (let first = 1001; first >= 551; first -= 50) {
    const anchor = ui.page.locator(`[data-message-id="m${first}"]`);
    await scroll.evaluate((el) => {
      el.scrollTop = 0;
    });
    await ui.page
      .getByText("Loading older messages…", { exact: true })
      .waitFor();
    const top = await anchor.evaluate((el) => el.getBoundingClientRect().top);
    await ui.page.locator(`[data-message-id="m${first - 50}"]`).waitFor();
    const after = await anchor.evaluate((el) => el.getBoundingClientRect().top);
    assert.ok(
      Math.abs(after - top) <= 2,
      `cap anchor shifted ${after - top}px`,
    );
    assert.ok((await count()) <= 600);
  }
  assert.equal(await count(), 600);
  const jump = ui.page.getByRole("button", {
    name: "Jump to latest",
    exact: true,
  });
  await jump.waitFor();
  const requests = ui.historyRequests.length;
  value.frame.messages = rows(1101, 1250);
  value.frame.messages[0].legs = [{ to: "agent", stage: { stage: "held" } }];
  ui.connections[0].send("space", value);
  await ui.page.waitForTimeout(160);
  assert.equal(await count(), 600);
  assert.equal(await ui.page.locator('[data-message-id="m1250"]').count(), 0);
  assert.equal(await ui.page.locator(".message-gap").count(), 0);
  assert.equal(ui.historyRequests.length, requests);
  await jump.click();
  await ui.page.locator('[data-message-id="m1250"]').waitFor();
  assert.equal(await count(), 150);
  assert.equal(await jump.count(), 0);
  assert.equal(
    await scroll.evaluate(
      (el) => el.scrollHeight - el.scrollTop - el.clientHeight < 2,
    ),
    true,
  );
  await scroll.evaluate((el) => {
    el.scrollTop = 0;
  });
  await ui.page.locator('[data-message-id="m1051"]').waitFor();
  assert.equal(await count(), 200);
});

test("UI-2 fix1: live frames format timestamps only for changed message rows", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = Array.from({ length: 150 }, (_, n) =>
    message(`row${n}`),
  );
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  await ui.page.evaluate(() => {
    window.timestampFormats = 0;
    const format = Date.prototype.toLocaleTimeString;
    Date.prototype.toLocaleTimeString = function (...args) {
      window.timestampFormats++;
      return format.apply(this, args);
    };
  });
  value.frame.messages[70].legs = [{ to: "agent", stage: { stage: "held" } }];
  ui.connections[0].send("space", value);
  await ui.page
    .locator('[data-message-id="row70"] [data-stage="held"]')
    .waitFor();
  assert.equal(await ui.page.evaluate(() => window.timestampFormats), 1);
  const measures = await ui.page.evaluate(() => {
    window.timestampFormats = 0;
    return performance.getEntriesByName("namedrop:frame-apply").length;
  });
  value.frame.participants[0].activity =
    value.frame.participants[0].activity === "working" ? "idle" : "working";
  ui.connections[0].send("space", value);
  await ui.page.waitForFunction(
    (before) =>
      performance.getEntriesByName("namedrop:frame-apply").length > before,
    measures,
  );
  await ui.page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
  assert.equal(await ui.page.evaluate(() => window.timestampFormats), 0);
});

test("design-b: landing follows the URL, then Needs you, and selection keeps the URL, Back and the tab title", async (t) => {
  const frames = () => [
    frame(1, "alpha"),
    frame(2, "beta"),
    frame(3, "gamma", "needs_you"),
  ];
  const requested = await open(
    t,
    (client) => replay(client, frames()),
    undefined,
    {
      path: `/w/${key(2)}`,
    },
  );
  await requested.live();
  assert.equal(await requested.row(2).getAttribute("aria-current"), "page");
  assert.equal(
    await requested.page.evaluate(() => location.pathname),
    `/w/${key(2)}`,
  );
  await requested.page.waitForFunction(
    () => document.title === "(1) Needs you · Namedrop",
  );
  const entries = await requested.page.evaluate(() => history.length);
  await requested.row(1).click();
  assert.equal(
    await requested.page.evaluate(() => location.pathname),
    `/w/${key(1)}`,
  );
  assert.equal(
    await requested.page.evaluate(() => history.length),
    entries + 1,
  );
  await requested.page.goBack();
  await requested.page.waitForFunction(
    (selected) =>
      document
        .querySelector(`.channels-navigation [data-channel-key="${selected}"]`)
        ?.getAttribute("aria-current") === "page",
    key(2),
  );
  requested.connections[0].send("space", frame(3, "gamma"));
  await requested.page.waitForFunction(() => document.title === "Namedrop");

  const landing = await open(t, (client) => replay(client, frames()));
  await landing.live();
  assert.equal(await landing.row(3).getAttribute("aria-current"), "page");
  assert.equal(
    await landing.page.evaluate(() => location.pathname),
    `/w/${key(3)}`,
  );
  assert.equal(
    await landing.page.evaluate(
      () => history.length === window.initialHistoryLength,
    ),
    true,
    "automatic landing replaces the URL instead of pushing",
  );
});

test("design-b: a channel is read after 1.5 seconds on screen, and arrow keys passing over keep it unread", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [frame(1, "alpha"), frame(2, "beta"), frame(3, "gamma")]),
  );
  await ui.live();
  ui.connections[0].send("space", activity(2, "beta"));
  ui.connections[0].send("space", activity(3, "gamma"));
  const unread = (n) => ui.row(n).getByRole("img", { name: "Unread activity" });
  await unread(2).waitFor();
  await unread(3).waitFor();
  await ui.row(1).focus();
  await ui.page.keyboard.press("ArrowDown");
  await ui.page.keyboard.press("ArrowDown");
  assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
  assert.equal(await unread(2).count(), 1);
  assert.equal(await unread(3).count(), 1);
  const selected = Date.now();
  await unread(3).waitFor({ state: "detached" });
  assert.ok(Date.now() - selected >= 1400, "read before the dwell elapsed");
  assert.equal(await unread(2).count(), 1);
});
