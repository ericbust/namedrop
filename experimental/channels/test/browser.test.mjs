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

const COOKIE = "namedrop_test";
function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => resolve(body));
    req.on("error", reject);
  });
}
function sendJson(res, status, value) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(value));
}
// Mirrors ND-4's POST /w/<key>/messages: cookie, CSRF header, JSON body, idempotent ids.
function postRoute(onPost) {
  const session = { secret: "session-1", csrf: "csrf-1", tokenReads: 0 };
  const attempts = [];
  const receipts = new Map();
  const authorized = (req) =>
    (req.headers.cookie ?? "")
      .split(/;\s*/)
      .includes(`${COOKIE}=${session.secret}`);
  return {
    session,
    attempts,
    receipts,
    cookie: `${COOKIE}=${session.secret}; Path=/; HttpOnly; SameSite=Strict`,
    rotate() {
      session.csrf = `csrf-${Number(session.csrf.slice(5)) + 1}`;
    },
    token(req, res) {
      session.tokenReads++;
      if (!authorized(req)) return sendJson(res, 401, {});
      sendJson(res, 200, { csrf_token: session.csrf });
    },
    async post(req, res, key) {
      const raw = await readBody(req);
      const attempt = {
        key,
        csrf: req.headers["x-namedrop-csrf"] ?? null,
        type: req.headers["content-type"] ?? null,
        raw,
        body: null,
        status: 0,
      };
      attempts.push(attempt);
      const reply = (status, value) => {
        attempt.status = status;
        sendJson(res, status, value);
      };
      if (!authorized(req)) return reply(401, { error: "No session." });
      if (attempt.csrf !== session.csrf) {
        attempt.status = 403;
        res.writeHead(403, { "Content-Type": "text/plain" });
        return res.end("Forbidden.\n");
      }
      if (attempt.type !== "application/json")
        return reply(400, { error: "Expected JSON." });
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        return reply(400, { error: "Invalid JSON." });
      }
      const fields = Object.keys(body);
      if (
        fields.some(
          (field) => !["request_id", "source", "in_reply_to"].includes(field),
        ) ||
        typeof body.request_id !== "string" ||
        typeof body.source !== "string" ||
        !body.source.trim() ||
        ("in_reply_to" in body && typeof body.in_reply_to !== "string")
      )
        return reply(400, { error: "Bad request." });
      attempt.body = body;
      const post = {
        key,
        request_id: body.request_id,
        source: body.source,
        in_reply_to: body.in_reply_to ?? null,
      };
      const known = receipts.get(post.request_id);
      if (known !== undefined)
        return JSON.stringify(known.post) === JSON.stringify(post)
          ? reply(200, known.receipt)
          : reply(400, {
              error: "That request id was used for another message.",
            });
      const outcome = await onPost(post);
      if (outcome.status !== 200)
        return reply(outcome.status, { error: outcome.error ?? "Refused." });
      const message_id = `posted-${receipts.size + 1}`;
      const receipt = {
        message_id,
        thread_id: post.in_reply_to ?? message_id,
        request_id: post.request_id,
      };
      receipts.set(post.request_id, { post, receipt });
      reply(200, receipt);
    },
    postedRow(requestId, at = Date.now()) {
      const { post, receipt } = receipts.get(requestId);
      return message(receipt.message_id, post.source, {
        thread_id: receipt.thread_id,
        in_reply_to: post.in_reply_to,
        at_ms: at,
        kind: post.in_reply_to === null ? "mention" : "reply",
        legs: [{ to: "codex", stage: { stage: "delivered" } }],
      });
    },
  };
}
async function open(
  t,
  onConnect,
  history = () => [],
  { path = "/", onPost = () => ({ status: 200 }) } = {},
) {
  const clients = new Set();
  const connections = [];
  const errors = [];
  const historyRequests = [];
  const posts = postRoute(onPost);
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
    if (url.pathname === "/api/session") return posts.token(req, res);
    const postKey = /^\/w\/([^/]+)\/messages$/.exec(url.pathname)?.[1];
    if (req.method === "POST" && postKey !== undefined)
      return posts.post(req, res, postKey);
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
        ...(page ? { "Set-Cookie": posts.cookie } : {}),
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
  return { page, channels, row, connections, live, historyRequests, posts };
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
  const banner = ui.page.getByRole("region", { name: "Attention needed" });
  await banner
    .getByText("Waiting on approval or a question", { exact: true })
    .waitFor();
  await banner.getByText("Open its pane in Herdr to answer.").waitFor();
  assert.equal(
    await ui.page
      .locator('.members .presence[data-activity="blocked"]')
      .count(),
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
  await banner.getByText("Something needs you", { exact: true }).waitFor();
  await banner.getByRole("button", { name: "Details" }).click();
  await banner.getByText("New future request…", { exact: true }).waitFor();
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
  const chip = ui.page.locator(".members .member");
  await chip.getByText("Unknown", { exact: true }).waitFor();
  assert.equal(await chip.getAttribute("title"), "Member");
  const unknown = await ui.page
    .locator('.members .presence[data-activity="unknown"]')
    .evaluate((dot) => {
      const style = getComputedStyle(dot);
      return [style.outlineStyle, style.backgroundColor];
    });
  assert.equal(unknown[0], "dashed");
  const offline = frame(2, "beta");
  offline.frame.participants[0].activity = "offline";
  ui.connections[0].send("space", offline);
  await ui.row(2).click();
  const offlineColor = await ui.page
    .locator('.members .presence[data-activity="offline"]')
    .evaluate((dot) => getComputedStyle(dot).backgroundColor);
  assert.notEqual(offlineColor, unknown[1]);
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
  await rows.nth(0).getByRole("button", { name: "Details" }).click();
  update.frame.attention[0].summary.text = "First request changed";
  ui.connections[0].send("space", update);
  await rows.nth(0).getByRole("button", { name: "Details" }).click();
  await rows.getByText("First request changed", { exact: true }).waitFor();
  assert.equal(await rows.count(), 2);
  assert.match(
    await rows.nth(1).textContent(),
    /^Something needs you.*Open Details for the full message\.Details$/,
  );
  for (const level of ["future_health", "paused", "inactive"]) {
    update.frame.health = { level };
    ui.connections[0].send("space", update);
    await ui.page
      .getByRole("region", { name: "Attention needed" })
      .waitFor({ state: "detached" });
    await ui.page.waitForFunction(
      (expected) =>
        (document.querySelector(".channel-header .health")?.textContent ??
          null) === expected,
      {
        future_health: null,
        paused: "Paused · deliveries held",
        inactive: "Not active",
      }[level],
    );
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
    "Connecting…",
  );
  assert.equal(
    await ui.page.getByText("No workspaces yet", { exact: true }).count(),
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
    "Connecting…",
  );
  ui.connections[1].send("ready", { keys: [] });
  await ui.page
    .getByRole("heading", { name: "No workspaces yet", exact: true })
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
    .getByRole("heading", { name: "No workspaces yet", exact: true })
    .waitFor();
  assert.equal(await ui.page.locator(".sidebar-footer").textContent(), "Live");
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
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  assert.equal(await timeline.locator("[data-message-id]").count(), 1);
  const button = timeline.getByRole("button", {
    name: /^1 reply from You at /,
  });
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
      .getByRole("button", { name: /^2 replies, latest from You at / })
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
  assert.equal(await ui.page.locator(".msg img, .msg script").count(), 0);
  assert.equal(dialogs, 0);
  const code = ui.page.locator(".body pre code");
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
  assert.equal(await ui.page.locator('.msg a[href^="javascript:"]').count(), 0);
  const long = ui.page.locator('[data-message-id="long"]');
  assert.equal(
    (await long.locator(".body").textContent()).includes("END OF BODY"),
    false,
  );
  await long.getByRole("button", { name: "Show more", exact: true }).click();
  assert.equal(
    (await long.locator(".body").textContent()).includes("END OF BODY"),
    true,
  );
  await long.getByText("Shortened by Namedrop.", { exact: true }).waitFor();
  await ui.page.getByText("Future messages survive", { exact: true }).waitFor();
  await ui.page.getByText("Status unknown", { exact: true }).waitFor();
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
  const scroll = ui.page.locator(".timeline");
  const first = ui.page.locator('[data-message-id="m101"]');
  await scroll.evaluate((element) => {
    element.scrollTop = 0;
  });
  await ui.page
    .getByText("Loading earlier messages…", { exact: true })
    .waitFor();
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
  assert.equal(await ui.page.locator(".divider.gap").count(), 0);
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
    (top) => document.querySelector(".timeline").scrollTop < top,
    before,
  );
});

test("UI-2 fix1: every delivery stage shows its visible receipt label", async (t) => {
  const value = frame(1, "alpha");
  const stages = [
    [{ stage: "queued" }, "Queued", null],
    [{ stage: "delivered" }, "Delivered", null],
    [{ stage: "working", acked_at_ms: 100 }, "Working on it", null],
    [{ stage: "replied", reply_id: "reply" }, "Replied", null],
    [{ stage: "completed", kind: "settled" }, "Done", null],
    [{ stage: "completed", kind: "no_reply" }, "Done, no reply needed", null],
    [{ stage: "completed", kind: "success" }, "Done", "success"],
    [{ stage: "held" }, "Held while paused", null],
    [{ stage: "failed" }, "Not delivered", null],
    [{ stage: "cancelled" }, "Cancelled", null],
    [{ stage: "future_stage" }, "Status unknown", null],
  ];
  value.frame.messages = stages.map(([stage], n) =>
    message(`stage${n}`, `Delivery ${n}`, { legs: [{ to: "agent", stage }] }),
  );
  value.frame.messages.push(
    message("several", "Two recipients", {
      legs: [
        { to: "agent", stage: { stage: "completed", kind: "settled" } },
        { to: "human", stage: { stage: "working", acked_at_ms: 100 } },
      ],
    }),
  );
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  for (const [index, [, label, title]] of stages.entries()) {
    const receipt = ui.page.locator(
      `[data-message-id="stage${index}"] .receipt li`,
    );
    await receipt.waitFor();
    assert.equal(await receipt.textContent(), label);
    assert.equal(await receipt.getAttribute("title"), title);
    assert.equal(await receipt.locator("svg").count(), 1);
  }
  assert.deepEqual(
    await ui.page
      .locator('[data-message-id="several"] .receipt li')
      .allTextContents(),
    ["an unknown agentDone", "YouWorking on it"],
  );
  assert.equal(
    await ui.page.getByText(/settled|no_reply|Completed/).count(),
    0,
  );
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
  assert.equal((await row.locator(".body").textContent()).endsWith("…"), true);
  const unicode = ui.page.locator('[data-message-id="unicode"]');
  assert.equal(
    await unicode.locator(".body").textContent(),
    `${"x".repeat(479)}…`,
  );
  await unicode.getByRole("button", { name: "Show more", exact: true }).click();
  assert.equal(
    await unicode.locator(".body").textContent(),
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
    .getByRole("button", { name: /^1 reply from You at .*\. Open thread$/ });
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

test("thread pane: with the thread open, focus on the timeline or the current sidebar row survives a live frame", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [exchange(), frame(2, "beta")]),
  );
  await ui.live();
  await ui.page.locator(".timeline .reply-preview").first().click();
  await ui.page.locator("aside.thread").waitFor();
  for (const [label, focus] of [
    ["timeline", () => ui.page.locator(".timeline").focus()],
    ["sidebar", () => ui.row(1).focus()],
  ]) {
    await focus();
    const value = exchange();
    value.frame.messages.push(message(`live-${label}`, "live", { at_ms: 500 }));
    ui.connections[0].send("space", value);
    await ui.page
      .locator(`.timeline [data-message-id="live-${label}"]`)
      .waitFor();
    await nextFrames(ui.page);
    assert.equal(
      await ui.page.evaluate(
        () => !!document.activeElement?.closest("aside.thread"),
      ),
      false,
      `${label} focus taken by the pane`,
    );
  }
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
  const scroll = ui.page.locator(".timeline");
  const count = () => ui.page.locator(".timeline [data-message-id]").count();
  for (let first = 1001; first >= 551; first -= 50) {
    const anchor = ui.page.locator(`[data-message-id="m${first}"]`);
    await scroll.evaluate((el) => {
      el.scrollTop = 0;
    });
    await ui.page
      .getByText("Loading earlier messages…", { exact: true })
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
  assert.equal(await ui.page.locator(".divider.gap").count(), 0);
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

function numberedRows(from, to, extra = () => ({})) {
  return Array.from({ length: to - from + 1 }, (_, n) =>
    message(`m${from + n}`, `Message ${from + n}`, extra(from + n)),
  );
}
async function loadPages(ui, pages) {
  for (let page = 1; page <= pages; page++) {
    await ui.scroll.evaluate((el) => {
      el.scrollTop = 0;
    });
    await ui.scroll
      .locator(`[data-message-id="m${1851 - page * 50}"]`)
      .waitFor();
  }
  await nextFrames(ui.page);
}
async function pagedChannel(t, pages, extra) {
  const value = frame(1, "alpha");
  value.frame.messages = numberedRows(1851, 2000, extra);
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    async (url) => {
      const before = Number(url.searchParams.get("before").slice(1));
      return numberedRows(before - 50, before - 1, extra).reverse();
    },
  );
  await ui.page.setViewportSize({ width: 1440, height: 900 });
  await ui.live();
  const paged = { ...ui, value, scroll: ui.page.locator(".timeline") };
  await loadPages(paged, pages);
  return paged;
}
const threadAt = (parent) => (n) =>
  n === parent
    ? { thread_id: `m${parent}` }
    : n === parent + 1
      ? {
          thread_id: `m${parent}`,
          in_reply_to: `m${parent}`,
          kind: "reply",
          author: "codex",
        }
      : {};
function nextFrames(page) {
  return page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
}
function firstVisible(scroll) {
  return scroll.evaluate((el) => {
    const top = el.getBoundingClientRect().top;
    const row = [...el.querySelectorAll("[data-message-id]")].find(
      (row) => row.getBoundingClientRect().bottom > top + 1,
    );
    return {
      id: row.dataset.messageId,
      offset: row.getBoundingClientRect().top - top,
    };
  });
}
function jumpControls(scroll) {
  return scroll.evaluate((el) => {
    const port = el.getBoundingClientRect();
    return [...el.querySelectorAll("button")]
      .map((button) => ({
        name: button.getAttribute("aria-label") ?? button.textContent,
        rect: button.getBoundingClientRect(),
      }))
      .filter(({ name }) => name.startsWith("Jump to latest"))
      .map(({ name, rect }) => ({
        name,
        inside: rect.top >= port.top && rect.bottom <= port.bottom,
      }));
  });
}
function pillPlacement(scroll) {
  return scroll.evaluate((el) => {
    const pill = el
      .querySelector('[aria-label="Jump to latest messages"]')
      .getBoundingClientRect();
    const end = [...el.querySelectorAll("button")]
      .find(
        (button) =>
          button.textContent === "Jump to latest" &&
          !button.hasAttribute("aria-label"),
      )
      ?.parentElement.getBoundingClientRect();
    return {
      gap: Math.round(el.getBoundingClientRect().bottom - pill.bottom),
      clearsEndRow: end === undefined || end.bottom <= pill.top,
    };
  });
}
const atBottom = (scroll) =>
  scroll.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight < 2);

test("history window: a live frame at the cap keeps the row being read in place", async (t) => {
  const ui = await pagedChannel(t, 9);
  assert.equal(
    await ui.page.locator(".timeline [data-message-id]").count(),
    600,
  );
  await ui.scroll.evaluate((el) => {
    el.scrollTop = 120;
  });
  await nextFrames(ui.page);
  const reading = await firstVisible(ui.scroll);
  ui.value.frame.messages = numberedRows(1856, 2005);
  ui.connections[0].send("space", ui.value);
  await ui.page
    .locator('[data-message-id="m2005"], .window-end')
    .first()
    .waitFor({ state: "attached" });
  await nextFrames(ui.page);
  const row = ui.page.locator(`[data-message-id="${reading.id}"]`);
  assert.equal(
    await row.count(),
    1,
    `${reading.id} was dropped while being read`,
  );
  const after = await firstVisible(ui.scroll);
  assert.equal(after.id, reading.id);
  assert.ok(
    Math.abs(after.offset - reading.offset) <= 2,
    `the row being read moved ${after.offset - reading.offset}px`,
  );
  assert.equal(await ui.page.locator('[data-message-id="m2005"]').count(), 0);
});

test("history window: while browsing, Jump to latest waits at the bottom and returns without loading history", async (t) => {
  const ui = await pagedChannel(t, 10);
  ui.value.frame.messages = numberedRows(1856, 2005);
  ui.connections[0].send("space", ui.value);
  await ui.page.waitForTimeout(160);
  assert.deepEqual(
    await ui.page
      .locator(".history-edge button")
      .evaluateAll((buttons) => buttons.map((button) => button.textContent)),
    ["Load earlier messages"],
  );
  const requests = ui.historyRequests.length;
  await ui.page.mouse.move(700, 500);
  for (let step = 0; step < 100 && !(await atBottom(ui.scroll)); step++) {
    await ui.page.mouse.wheel(0, 2000);
    await nextFrames(ui.page);
  }
  assert.equal(await atBottom(ui.scroll), true);
  assert.deepEqual(await jumpControls(ui.scroll), [
    { name: "Jump to latest", inside: true },
    { name: "Jump to latest messages", inside: true },
  ]);
  assert.equal(ui.historyRequests.length, requests);
  assert.deepEqual(await pillPlacement(ui.scroll), {
    gap: 16,
    clearsEndRow: true,
  });
  await ui.page
    .getByRole("button", { name: "Jump to latest messages" })
    .click();
  await ui.page.locator('[data-message-id="m2005"]').waitFor();
  await nextFrames(ui.page);
  assert.equal(await atBottom(ui.scroll), true);
  assert.deepEqual(await jumpControls(ui.scroll), []);
  assert.equal(ui.historyRequests.length, requests);
});

test("history window: a screen above the newest message, the pill scrolls back to it", async (t) => {
  const ui = await pagedChannel(t, 0);
  const pill = ui.page.getByRole("button", { name: "Jump to latest messages" });
  assert.equal(await pill.count(), 0);
  await ui.scroll.evaluate((el) => {
    el.scrollTop = el.scrollHeight - el.clientHeight * 1.5;
  });
  await nextFrames(ui.page);
  assert.equal(await pill.count(), 0);
  await ui.scroll.evaluate((el) => {
    el.scrollTop = el.scrollHeight - el.clientHeight * 3;
  });
  await nextFrames(ui.page);
  assert.deepEqual(await jumpControls(ui.scroll), [
    { name: "Jump to latest messages", inside: true },
  ]);
  assert.deepEqual(await pillPlacement(ui.scroll), {
    gap: 16,
    clearsEndRow: true,
  });
  await pill.click();
  await ui.page.waitForFunction(
    () =>
      document.querySelector('[aria-label="Jump to latest messages"]') === null,
  );
  assert.equal(await atBottom(ui.scroll), true);
  assert.equal(
    await ui.page.locator(".timeline [data-message-id]").count(),
    150,
  );
  ui.value.frame.messages = numberedRows(1856, 2005);
  ui.connections[0].send("space", ui.value);
  await ui.page.locator('[data-message-id="m2005"]').waitFor();
  await nextFrames(ui.page);
  assert.equal(await atBottom(ui.scroll), true);
});

test("thread pane: a thread that Jump to latest unloads closes for good, and focus lands on the timeline", async (t) => {
  const ui = await pagedChannel(t, 12, threadAt(1300));
  const pane = ui.page.locator("aside.thread");
  const preview = ui.page.locator(
    '.timeline [data-message-id="m1300"] .reply-preview',
  );
  await preview.click();
  await pane.waitFor();
  await ui.page
    .getByRole("button", { name: "Jump to latest messages" })
    .click();
  await pane.waitFor({ state: "detached" });
  const focus = await ui.page.evaluate(() =>
    document.activeElement?.matches("section.timeline")
      ? "timeline"
      : document.activeElement?.tagName,
  );
  await loadPages(ui, 12);
  await preview.waitFor();
  await nextFrames(ui.page);
  assert.equal(await pane.count(), 0, "closed pane reopened by itself");
  assert.equal(await preview.getAttribute("aria-expanded"), "false");
  assert.equal(focus, "timeline");
});

test("thread pane: a thread the history cap evicts closes and moves focus from the pane to the timeline", async (t) => {
  const ui = await pagedChannel(t, 0, threadAt(1999));
  const pane = ui.page.locator("aside.thread");
  await ui.page
    .locator('.timeline [data-message-id="m1999"] .reply-preview')
    .click();
  await pane.waitFor();
  assert.equal(
    await pane
      .getByRole("button", { name: "Close thread" })
      .evaluate((el) => el === document.activeElement),
    true,
  );
  await loadPages(ui, 10);
  await pane.waitFor({ state: "detached" });
  assert.equal(
    await ui.page.evaluate(() =>
      document.activeElement?.matches("section.timeline"),
    ),
    true,
    "focus moved to the timeline, not the body",
  );
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
    "/",
    "automatic landing never rewrites the URL",
  );
  assert.equal(
    await landing.page.evaluate(
      () => history.length === window.initialHistoryLength,
    ),
    true,
  );
});

function selected(ui, n) {
  return ui.page.waitForFunction(
    (key) =>
      document
        .querySelector(`.channels-navigation [data-channel-key="${key}"]`)
        ?.getAttribute("aria-current") === "page",
    key(n),
  );
}
test("landing: late frames select the URL's channel when it arrives, and an automatic choice never rewrites the URL", async (t) => {
  const requested = await open(t, () => {}, undefined, {
    path: `/w/${key(2)}`,
  });
  const path = (ui) => ui.page.evaluate(() => location.pathname);
  const pushed = (ui) =>
    ui.page.evaluate(() => history.length - window.initialHistoryLength);
  await requested.page.waitForTimeout(2300);
  requested.connections[0].send("space", frame(1, "alpha"));
  await selected(requested, 1);
  assert.equal(await path(requested), `/w/${key(2)}`);
  requested.connections[0].send("space", frame(3, "gamma", "needs_you"));
  await selected(requested, 3);
  assert.equal(await path(requested), `/w/${key(2)}`);
  requested.connections[0].send("space", frame(2, "beta"));
  await selected(requested, 2);
  assert.equal(await path(requested), `/w/${key(2)}`);
  assert.equal(await pushed(requested), 0);
  requested.connections[0].send("space", frame(1, "alpha", "needs_you"));
  await requested.page.waitForFunction(
    () => document.title === "(2) Needs you · Namedrop",
  );
  assert.equal(await requested.row(2).getAttribute("aria-current"), "page");
  requested.connections[0].send("ready", { keys: [key(1), key(2), key(3)] });
  await requested.live();
  assert.equal(await requested.row(2).getAttribute("aria-current"), "page");
  assert.equal(await path(requested), `/w/${key(2)}`);

  const root = await open(t, () => {});
  await root.page.waitForTimeout(2300);
  root.connections[0].send("space", frame(1, "alpha"));
  await selected(root, 1);
  assert.equal(await path(root), "/");
  root.connections[0].send("space", frame(3, "gamma", "needs_you"));
  await selected(root, 3);
  assert.equal(await path(root), "/");
  assert.equal(await pushed(root), 0);
  root.connections[0].send("ready", { keys: [key(1), key(3)] });
  await root.live();
  assert.equal(await root.row(3).getAttribute("aria-current"), "page");
  assert.equal(await path(root), "/");
  await root.row(1).click();
  assert.equal(await path(root), `/w/${key(1)}`);
  assert.equal(await pushed(root), 1);
  root.connections[0].send("space", frame(4, "delta", "needs_you"));
  await root.row(4).waitFor();
  assert.equal(await root.row(1).getAttribute("aria-current"), "page");
  assert.equal(await path(root), `/w/${key(1)}`);
  await root.page.goBack();
  await root.page.waitForFunction(() => location.pathname === "/");
  await nextFrames(root.page);
  assert.equal(
    await root.channels
      .locator('[aria-current="page"]')
      .getAttribute("data-channel-key"),
    key(4),
    "Back to the root lands again on the first channel that needs you",
  );
  await root.page.goForward();
  await selected(root, 1);
  assert.equal(await path(root), `/w/${key(1)}`);
});

test("landing: after ready, a workspace that opens later or a reconnect replay never moves the reader or closes the thread", async (t) => {
  const alpha = exchange();
  for (const row of alpha.frame.messages) row.at_ms = Date.now() - 1000;
  const later = frame(5, "epsilon");
  later.frame.messages = [message("later", "newest", { at_ms: Date.now() })];
  const ui = await open(t, (client, n) =>
    replay(
      client,
      n === 1
        ? [alpha, frame(2, "beta")]
        : [alpha, frame(2, "beta"), later, frame(7, "eta", "needs_you")],
    ),
  );
  await ui.live();
  assert.equal(await ui.row(1).getAttribute("aria-current"), "page");
  await ui.page.locator(".timeline .reply-preview").first().click();
  const pane = ui.page.locator("aside.thread");
  await pane.waitFor();
  ui.connections[0].send("space", later);
  await ui.row(5).waitFor();
  assert.equal(
    await ui.row(1).getAttribute("aria-current"),
    "page",
    "a new calm workspace moved the reader",
  );
  assert.equal(await pane.count(), 1, "a new workspace closed the thread");
  ui.connections[0].send("space", frame(6, "zeta", "needs_you"));
  await ui.row(6).waitFor();
  assert.equal(
    await ui.row(1).getAttribute("aria-current"),
    "page",
    "a new workspace that needs you moved the reader",
  );
  assert.equal(await pane.count(), 1);
  ui.connections[0].destroy();
  await ui.page.getByRole("status").waitFor();
  await ui.row(7).waitFor();
  await ui.live();
  assert.equal(
    await ui.row(1).getAttribute("aria-current"),
    "page",
    "a reconnect replay moved the reader",
  );
  assert.equal(await pane.count(), 1, "a reconnect replay closed the thread");
});

test("landing: reader input ends discovery, and after ready the URL's channel is still chosen when it arrives", async (t) => {
  const touched = await open(t, () => {});
  await touched.page.waitForTimeout(2300);
  touched.connections[0].send("space", frame(1, "alpha"));
  await selected(touched, 1);
  await touched.page.evaluate(() => {
    window.wheeled = new Promise((resolve) =>
      addEventListener("wheel", resolve, { once: true }),
    );
  });
  await touched.page.mouse.move(300, 300);
  await touched.page.mouse.wheel(0, 40);
  await touched.page.evaluate(() => window.wheeled);
  touched.connections[0].send("space", frame(3, "gamma", "needs_you"));
  await touched.row(3).waitFor();
  await nextFrames(touched.page);
  assert.equal(
    await touched.row(1).getAttribute("aria-current"),
    "page",
    "input during discovery did not stop the landing",
  );

  const requested = await open(
    t,
    (client) =>
      replay(client, [frame(1, "alpha"), frame(3, "gamma", "needs_you")]),
    undefined,
    { path: `/w/${key(2)}` },
  );
  await requested.live();
  assert.equal(await requested.row(3).getAttribute("aria-current"), "page");
  requested.connections[0].send("space", frame(2, "beta"));
  await selected(requested, 2);
  assert.equal(
    await requested.page.evaluate(() => location.pathname),
    `/w/${key(2)}`,
  );
  assert.equal(
    await requested.page.evaluate(
      () => history.length - window.initialHistoryLength,
    ),
    0,
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

function people() {
  const base = fixture.frame.participants[0];
  return [
    {
      ...base,
      address_id: "codex",
      alias: "@codex",
      kind: { type: "agent", runtime: "codex" },
      activity: "idle",
    },
    {
      ...base,
      address_id: "shell",
      alias: "@shell",
      kind: { type: "terminal" },
      activity: "idle",
    },
    {
      ...base,
      address_id: "human",
      alias: "you",
      kind: { type: "human" },
      activity: "waiting",
    },
  ];
}
const done = (kind = "settled") => ({ stage: "completed", kind });
function conversation() {
  const at = Date.now() - 10 * 60_000;
  const value = frame(1, "alpha");
  value.frame.participants = people();
  const reply = (id, thread, parent, author, text, offset, to = "human") =>
    message(id, text, {
      thread_id: thread,
      in_reply_to: parent,
      kind: "reply",
      author,
      at_ms: at + offset,
      legs: [{ to, stage: done("no_reply") }],
    });
  value.frame.messages = [
    message("ask", "please reply with the single word pong.", {
      thread_id: "t1",
      kind: "handoff",
      at_ms: at,
      legs: [{ to: "codex", stage: done() }],
    }),
    reply("r1", "t1", "ask", "codex", "first answer", 60_000),
    reply("r2", "t1", "ask", "codex", "pong", 120_000),
    message("cmd", "echo hello", {
      thread_id: "t2",
      at_ms: at + 180_000,
      legs: [{ to: "shell", stage: { stage: "replied", reply_id: "out" } }],
    }),
    reply(
      "out",
      "t2",
      "cmd",
      "shell",
      "@shell command finished with exit code 0.\nstdout:\nhello\n\nstderr:",
      190_000,
    ),
    message("cmd2", "false", {
      thread_id: "t3",
      at_ms: at + 200_000,
      legs: [{ to: "shell", stage: { stage: "replied", reply_id: "out2" } }],
    }),
    reply("out2", "t3", "cmd2", "shell", "exit code 1", 210_000),
    message("pending", "Summarize the open TODOs.", {
      thread_id: "t4",
      at_ms: at + 220_000,
      legs: [{ to: "codex", stage: { stage: "working", acked_at_ms: 1 } }],
    }),
  ];
  value.frame.threads = [
    {
      thread_id: "t1",
      title: { text: "t1", truncated: false },
      message_count: 3,
    },
  ];
  value.frame.terminal_runs = [
    {
      terminal: "shell",
      command: { text: "echo hello", truncated: false },
      started_at_ms: at + 180_000,
      exit_code: 0,
      message_id: "cmd",
    },
    {
      terminal: "shell",
      command: { text: "false", truncated: false },
      started_at_ms: at + 200_000,
      exit_code: 1,
      message_id: "something-else",
    },
  ];
  return { value, at };
}

test("design-b: requests show their latest reply inline, commands and output read as terminal text, and Exit needs a matching run", async (t) => {
  const { value, at } = conversation();
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  const time = await ui.page.evaluate(
    (ms) =>
      new Date(ms).toLocaleTimeString([], {
        hour: "numeric",
        minute: "2-digit",
      }),
    at + 120_000,
  );
  const preview = timeline.getByRole("button", {
    name: `2 replies, latest from @codex at ${time}. Open thread`,
    exact: true,
  });
  assert.equal(await preview.locator(".rp-body").textContent(), "pong");
  assert.equal(await preview.locator(".rp-count").textContent(), "2 replies");
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .receipt').count(),
    0,
  );
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .msg-head').textContent(),
    `You to @codexHandoff${await ui.page.evaluate(
      (ms) =>
        new Date(ms).toLocaleTimeString([], {
          hour: "numeric",
          minute: "2-digit",
        }),
      at,
    )}`,
  );
  assert.equal(
    await timeline
      .locator('[data-message-id="pending"] .receipt')
      .textContent(),
    "Working on it",
  );
  assert.equal(
    await timeline.locator('[data-message-id="cmd"] .command').textContent(),
    "echo hello",
  );
  const output = timeline.locator('[data-message-id="cmd"] .reply-preview');
  assert.equal(await output.locator(".rp-body.mono").textContent(), "hello");
  assert.equal(await output.locator(".exit").textContent(), "Exit 0");
  assert.equal(
    await timeline.locator('[data-message-id="cmd2"] .exit').count(),
    0,
  );
  assert.equal(await timeline.locator("[data-message-id]").count(), 4);
  await preview.click();
  const pane = ui.page.getByRole("complementary", { name: "Thread" });
  assert.equal(await pane.locator(".thread-where").textContent(), "in #alpha");
  assert.equal(await pane.locator(".divider").textContent(), "2 replies");
  assert.equal(await pane.locator("[data-message-id]").count(), 3);
  await output.click();
  await pane.locator('[data-message-id="out"] pre.output').waitFor();
  assert.equal(
    await pane.locator('[data-message-id="out"] pre.output').textContent(),
    "hello",
  );
  assert.equal(
    await pane.locator('[data-message-id="out"] .msg-head .exit').textContent(),
    "Exit 0",
  );
});

test("thread pane: each terminal reply shows its own run, and the preview shows the newest", async (t) => {
  const at = Date.now() - 10 * 60_000;
  const value = frame(1, "alpha");
  value.frame.participants = people();
  const result =
    "@shell command finished with exit code 5.\nstdout:\nout\n\nstderr:";
  const reply = (id, offset) =>
    message(id, result, {
      thread_id: "make",
      in_reply_to: "make",
      kind: "reply",
      author: "shell",
      at_ms: at + offset,
    });
  value.frame.messages = [
    message("make", "make", {
      thread_id: "make",
      at_ms: at,
      legs: [{ to: "shell", stage: done() }],
    }),
    reply("first", 1500),
    reply("second", 2500),
  ];
  const run = (exit_code, offset) => ({
    terminal: "shell",
    command: { text: "make", truncated: false },
    started_at_ms: at + offset,
    exit_code,
    message_id: "make",
  });
  value.frame.terminal_runs = [run(0, 2000), run(1, 1000)];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const preview = ui.page.locator(
    '.timeline [data-message-id="make"] .reply-preview',
  );
  assert.equal(await preview.locator(".exit").textContent(), "Exit 0");
  await preview.click();
  const pane = ui.page.locator("aside.thread");
  await pane.waitFor();
  assert.deepEqual(await pane.locator(".exit").allTextContents(), [
    "Exit 1",
    "Exit 0",
  ]);
});

test("thread pane: a non-terminal reply that mimics a terminal result renders as text", async (t) => {
  const value = frame(1, "alpha");
  value.frame.participants = people();
  const at = Date.now() - 600_000;
  value.frame.messages = [
    message("ask", "run it", {
      thread_id: "t",
      at_ms: at,
      legs: [{ to: "codex", stage: done() }],
    }),
    message(
      "answer",
      "@shell command finished with exit code 8.\nstdout:\nx\n\nstderr:",
      {
        thread_id: "t",
        in_reply_to: "ask",
        kind: "reply",
        author: "codex",
        at_ms: at + 500,
      },
    ),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  await ui.page
    .locator('.timeline [data-message-id="ask"] .reply-preview')
    .click();
  const reply = ui.page.locator('aside.thread [data-message-id="answer"]');
  await reply.waitFor();
  assert.equal(await reply.locator(".exit").count(), 0);
  assert.equal(await reply.locator("pre.output").count(), 0);
  assert.match(
    await reply.locator(".body").first().textContent(),
    /command finished/,
  );
});

test("design-b fix: terminal results show stdout, stderr and the exit code instead of the preamble", async (t) => {
  const at = Date.now() - 10 * 60_000;
  const value = frame(1, "alpha");
  value.frame.participants = people();
  const result = (code, output) =>
    `@shell command finished with exit code ${code}.\nstdout:\n${output}`;
  value.frame.messages = [
    ["ls", result(0, "a\nb\nc\n\nstderr:\n")],
    ["make", result(2, "partial\n\nstderr:\nboom: no rule\n")],
    ["true", result(0, "\nstderr:")],
    ["false", result(0, "x\n\nstderr:\n")],
  ].flatMap(([command, body], n) => [
    message(`c${n}`, command, {
      thread_id: `t${n}`,
      at_ms: at + n * 60_000,
      legs: [{ to: "shell", stage: done() }],
    }),
    message(`o${n}`, body, {
      thread_id: `t${n}`,
      in_reply_to: `c${n}`,
      kind: "reply",
      author: "shell",
      at_ms: at + n * 60_000 + 1000,
      legs: [{ to: "human", stage: done("no_reply") }],
    }),
  ]);
  value.frame.terminal_runs = [
    {
      terminal: "shell",
      command: { text: "false", truncated: false },
      started_at_ms: at + 3 * 60_000,
      exit_code: 1,
      message_id: "c3",
    },
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  const preview = (n) =>
    timeline.locator(`[data-message-id="c${n}"] .reply-preview`);
  const previews = [];
  for (const n of [0, 1, 2, 3])
    previews.push([
      await preview(n).locator(".exit").textContent(),
      await preview(n).locator(".rp-body").textContent(),
      await preview(n).locator(".rp-body.mono").count(),
    ]);
  assert.deepEqual(previews, [
    ["Exit 0", "a\nb", 1],
    ["Exit 2", "partial", 1],
    ["Exit 0", "No output", 0],
    ["Exit 1", "x", 1],
  ]);
  assert.equal(await timeline.getByText("command finished").count(), 0);
  const pane = ui.page.getByRole("complementary", { name: "Thread" });
  const shown = async (n) => {
    await preview(n).click();
    const reply = pane.locator(`[data-message-id="o${n}"]`);
    await reply.waitFor();
    return {
      exit: await reply.locator(".msg-head .exit").textContent(),
      blocks: await reply.locator("pre.output").allTextContents(),
      stderr: await reply.locator(".stderr-label").allTextContents(),
      empty: await reply.locator(".no-output").allTextContents(),
    };
  };
  assert.deepEqual(await shown(0), {
    exit: "Exit 0",
    blocks: ["a\nb\nc"],
    stderr: [],
    empty: [],
  });
  assert.deepEqual(await shown(1), {
    exit: "Exit 2",
    blocks: ["partial", "boom: no rule\n"],
    stderr: ["stderr"],
    empty: [],
  });
  assert.deepEqual(await shown(2), {
    exit: "Exit 0",
    blocks: [],
    stderr: [],
    empty: ["No output"],
  });
  assert.deepEqual(await shown(3), {
    exit: "Exit 1",
    blocks: ["x"],
    stderr: [],
    empty: [],
  });
  assert.equal(await pane.getByText("command finished").count(), 0);
});

test("design-b fix: the message hover keeps the hover token's hue in both themes", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [message("row", "Hover me")];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  for (const colorScheme of ["light", "dark"]) {
    await ui.page.emulateMedia({ colorScheme });
    const row = ui.page.locator('[data-message-id="row"]');
    await row.hover();
    const { hovered, expected } = await row.evaluate((element) => {
      const canvas = document.createElement("canvas");
      canvas.width = 1;
      canvas.height = 1;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      const paint = (...layers) => {
        context.clearRect(0, 0, 1, 1);
        for (const [color, alpha] of layers) {
          context.globalAlpha = alpha;
          context.fillStyle = color;
          context.fillRect(0, 0, 1, 1);
        }
        return [...context.getImageData(0, 0, 1, 1).data.slice(0, 3)];
      };
      let behind = element.parentElement;
      while (getComputedStyle(behind).backgroundColor === "rgba(0, 0, 0, 0)")
        behind = behind.parentElement;
      const page = getComputedStyle(behind).backgroundColor;
      const probe = document.createElement("div");
      probe.style.background = "var(--bg-hover)";
      document.body.append(probe);
      const token = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return {
        hovered: paint(
          [page, 1],
          [getComputedStyle(element).backgroundColor, 1],
        ),
        expected: paint([page, 1], [token, 0.5]),
      };
    });
    for (const [index, channel] of hovered.entries())
      assert.ok(
        Math.abs(channel - expected[index]) <= 2,
        `${colorScheme} hover ${hovered} should match ${expected}`,
      );
  }
});

test("design-b fix: fenced code sits between its lines without blank lines", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [
    message("code", "files:\n```\na\nb\n```\nReference one\n\nParagraph two"),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const body = ui.page.locator('[data-message-id="code"] .body');
  const layout = await body.evaluate((element) => {
    const top = (word) => {
      const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
      while (walker.nextNode()) {
        const at = walker.currentNode.textContent.indexOf(word);
        if (at < 0) continue;
        const range = document.createRange();
        range.setStart(walker.currentNode, at);
        range.setEnd(walker.currentNode, at + 1);
        return range.getBoundingClientRect();
      }
    };
    const pre = element.querySelector("pre").getBoundingClientRect();
    const line = Number.parseFloat(getComputedStyle(element).lineHeight);
    return {
      line,
      above: pre.top - top("files").bottom,
      below: top("Reference").top - pre.bottom,
      paragraph: top("Paragraph").top - top("Reference").top,
    };
  });
  assert.ok(layout.above < layout.line, `blank line above: ${layout.above}`);
  assert.ok(layout.below < layout.line, `blank line below: ${layout.below}`);
  assert.ok(
    layout.paragraph >= 2 * layout.line,
    `the author's own blank line stays: ${layout.paragraph}`,
  );
});

async function pixelColumn(page, clip) {
  const png = await page.screenshot({ clip: { ...clip, width: 1 } });
  return page.evaluate(async (data) => {
    const image = new Image();
    image.src = `data:image/png;base64,${data}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = image.height;
    const context = canvas.getContext("2d");
    context.drawImage(image, 0, 0);
    const pixels = context.getImageData(0, 0, 1, image.height).data;
    return Array.from({ length: image.height }, (_, row) => [
      ...pixels.slice(row * 4, row * 4 + 3),
    ]);
  }, png.toString("base64"));
}
test("design-b fix: the timeline fades content over its top 12px and keeps a full focus ring", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [
    message("tall", `\x60\x60\x60\n${"a\n".repeat(60)}\x60\x60\x60`),
    ...Array.from({ length: 20 }, (_, n) => message(`after${n}`)),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.page.setViewportSize({ width: 1024, height: 700 });
  await ui.live();
  const { x, y } = await ui.page.evaluate(() => {
    const timeline = document.querySelector(".timeline");
    const pre = document.querySelector('[data-message-id="tall"] pre');
    timeline.scrollTop +=
      pre.getBoundingClientRect().top -
      timeline.getBoundingClientRect().top +
      80;
    return {
      x: Math.round(pre.getBoundingClientRect().right - 8),
      y: Math.round(timeline.getBoundingClientRect().top),
    };
  });
  const sum = (color) => color.reduce((total, channel) => total + channel, 0);
  const column = await pixelColumn(ui.page, { x, y, height: 30 });
  assert.ok(
    sum(column[0]) > sum(column[29]) + 9,
    `the top edge fades to the page: ${column[0]} over ${column[29]}`,
  );
  const ring = await ui.page.evaluate(() => {
    const timeline = document.querySelector(".timeline");
    timeline.focus({ focusVisible: true });
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    context.fillStyle = getComputedStyle(timeline).outlineColor;
    context.fillRect(0, 0, 1, 1);
    return [...context.getImageData(0, 0, 1, 1).data.slice(0, 3)];
  });
  const [edge] = await pixelColumn(ui.page, { x, y, height: 1 });
  assert.ok(
    edge.every((channel, index) => Math.abs(channel - ring[index]) <= 2),
    `the focus ring's top edge ${edge} matches ${ring}`,
  );
});

test("design-b: loading, empty channel and history errors use the design copy", async (t) => {
  let fail = true;
  const loading = await open(t, () => {});
  const skeleton = loading.page.locator(".main .skeleton");
  await skeleton.waitFor({ state: "attached" });
  const first = await skeleton.evaluate((element) => ({
    visibility: getComputedStyle(element).visibility,
    time: Number(element.getAnimations()[0]?.currentTime ?? 0),
  }));
  if (first.time < 380) assert.equal(first.visibility, "hidden");
  await loading.page.waitForFunction(
    () =>
      getComputedStyle(document.querySelector(".main .skeleton")).visibility ===
      "visible",
  );
  assert.equal(
    await loading.page.getByRole("heading", { level: 1 }).textContent(),
    "Connecting to Namedrop",
  );
  assert.equal(
    await loading.page.locator(".sidebar .skeleton .skeleton-row").count(),
    3,
  );
  loading.connections[0].send("ready", { keys: [] });
  await loading.page
    .getByText(
      "Open a workspace in Herdr and name an agent with namedrop name. It shows up here as a channel.",
      { exact: true },
    )
    .waitFor();

  const empty = frame(2, "beta");
  empty.frame.messages = [];
  const full = frame(1, "alpha");
  full.frame.messages = [message("m9", "Latest", { at_ms: 900 })];
  const ui = await open(
    t,
    (client) => replay(client, [full, empty]),
    () => {
      if (fail) {
        fail = false;
        throw new Error("history down");
      }
      return [message("m1", "Earlier")];
    },
  );
  await ui.live();
  await ui.page
    .getByRole("button", { name: "Load earlier messages", exact: true })
    .click();
  await ui.page
    .getByText("Couldn't load earlier messages.", { exact: true })
    .waitFor();
  await ui.page.getByRole("button", { name: "Try again", exact: true }).click();
  await ui.page.getByText("Earlier", { exact: true }).waitFor();
  assert.equal(ui.historyRequests.length, 2);
  await ui.row(2).click();
  await ui.page
    .getByRole("heading", { name: "No messages yet", exact: true })
    .waitFor();
  await ui.page
    .getByText(
      "When agents in this workspace mention each other, the conversation shows up here.",
      { exact: true },
    )
    .waitFor();
});

test("design-b: Needs you shows titles, hints and ages three at a time and announces new items", async (t) => {
  const value = frame(1, "alpha", "needs_you");
  const opened = Date.now() - 4 * 60_000 - 5_000;
  const card = (source_kind, text, extra = {}) => ({
    source_kind,
    summary: { text, truncated: false },
    opened_at_ms: opened,
    ...extra,
  });
  value.frame.attention = [
    card(
      "delivery_attempt",
      "ND-DELIVERY-003: The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox.",
      { subject_alias: "codex", message_id: "m1" },
    ),
    card("agent_status", "@claude is waiting on an approval or question."),
    card("coordination", "Coordination ND-COORD-001 waits for close."),
    card("context_grant", "Context grant request pending."),
  ];
  const ui = await open(t, (client) =>
    replay(client, [value, frame(2, "beta")]),
  );
  await ui.live();
  const banner = ui.page.getByRole("region", { name: "Attention needed" });
  const items = banner.getByRole("listitem");
  assert.equal(await items.count(), 3);
  assert.deepEqual(await items.locator(".attention-title").allTextContents(), [
    "@codex didn't answer a request",
    "@claude is waiting on an approval or question.",
    "Coordination needs a decision",
  ]);
  assert.equal(
    await items.nth(0).locator(".attention-hint").textContent(),
    "It ended its turn without replying.",
  );
  assert.equal(
    await items.nth(0).locator(".attention-age").textContent(),
    "4 min",
  );
  assert.equal(
    await banner.getByText(/ND-DELIVERY|delivery\.inbox/).count(),
    0,
  );
  await items.nth(0).getByRole("button", { name: "Details" }).click();
  await banner.getByText(/delivery\.inbox/).waitFor();
  await banner
    .getByRole("button", { name: "Show 1 more", exact: true })
    .click();
  assert.equal(await items.count(), 4);
  assert.equal(
    await ui.page
      .getByRole("navigation", { name: "Needs you" })
      .locator(".row-reason")
      .textContent(),
    "@codex didn't answer a request · 3 more",
  );
  assert.equal(
    await banner.getByRole("button", { name: /^Show message/ }).count(),
    0,
  );
  assert.equal(await ui.page.locator(".channel-header .health").count(), 0);
  const live = ui.page.locator('[aria-live="polite"]');
  assert.equal(await live.textContent(), "");
  value.frame.attention.push(card("pane_binding", "Pane moved."));
  ui.connections[0].send("space", value);
  await ui.page.waitForFunction(
    () =>
      document.querySelector('[aria-live="polite"]')?.textContent ===
      "Needs you: An agent's pane changed",
  );
});

function locatedChannel() {
  const value = frame(1, "alpha", "needs_you");
  value.frame.participants = people();
  const at = Date.now() - 60 * 60_000;
  value.frame.messages = Array.from({ length: 60 }, (_, n) =>
    message(`m${n}`, `Request ${n}`, {
      at_ms: at + n * 60_000,
      legs: [{ to: "codex", stage: { stage: "delivered" } }],
    }),
  );
  const card = (source_kind, text, subject_alias, message_id) => ({
    source_kind,
    summary: { text, truncated: false },
    opened_at_ms: at,
    subject_alias,
    message_id,
  });
  value.frame.attention = [
    card("delivery_attempt", "No reply.", "codex", "m20"),
    card("agent_status", "@claude is waiting.", "claude", "older"),
    card("coordination", "Close it.", null, "m40"),
  ];
  return value;
}
async function centerOffset(timeline, id) {
  return timeline.evaluate((element, id) => {
    const row = element.querySelector(`[data-message-id="${id}"]`);
    const port = element.getBoundingClientRect();
    const box = row.getBoundingClientRect();
    return Math.abs(
      box.top +
        box.height / 2 -
        (port.top + element.clientTop + element.clientHeight / 2),
    );
  }, id);
}
async function tint(row) {
  return row.evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      located: element.classList.contains("located"),
      animation: style.animationName,
      background: style.backgroundColor,
    };
  });
}

test("Needs you: Show message centers the loaded request, tints it, opens its thread, and Escape returns to the button", async (t) => {
  const ui = await open(t, (client) => replay(client, [locatedChannel()]));
  await ui.page.setViewportSize({ width: 1440, height: 900 });
  await ui.live();
  const banner = ui.page.getByRole("region", { name: "Attention needed" });
  const items = banner.getByRole("listitem");
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.locator('[data-message-id="m59"]').waitFor();
  const show = items
    .nth(0)
    .getByRole("button", { name: "Show message from @codex", exact: true });
  assert.equal(await show.textContent(), "Show message");
  assert.equal(
    await items
      .nth(1)
      .getByRole("button", { name: /^Show message/ })
      .count(),
    0,
  );
  assert.equal(
    await items
      .nth(2)
      .getByRole("button", { name: "Show message", exact: true })
      .count(),
    1,
  );
  const row = timeline.locator('[data-message-id="m20"]');
  assert.ok((await centerOffset(timeline, "m20")) > 100, "starts far away");
  await show.click();
  const pane = ui.page.getByRole("complementary", { name: "Thread" });
  await pane.locator('[data-message-id="m20"]').waitFor();
  const offset = await centerOffset(timeline, "m20");
  assert.ok(offset <= 4, `row center is ${offset}px from the middle`);
  const tinted = await tint(row);
  assert.equal(tinted.located, true);
  assert.equal(tinted.animation, "located");
  assert.equal(
    await pane
      .getByRole("button", { name: "Close thread" })
      .evaluate((button) => button === document.activeElement),
    true,
  );
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  assert.equal(
    await show.evaluate((button) => button === document.activeElement),
    true,
  );
  await ui.page.waitForFunction(
    () =>
      !document
        .querySelector('.timeline [data-message-id="m20"]')
        .classList.contains("located"),
  );
  await ui.page.emulateMedia({ reducedMotion: "reduce" });
  await items
    .nth(2)
    .getByRole("button", { name: "Show message", exact: true })
    .click();
  await pane.locator('[data-message-id="m40"]').waitFor();
  const still = await tint(timeline.locator('[data-message-id="m40"]'));
  assert.deepEqual(still, {
    located: true,
    animation: "none",
    background: await ui.page.evaluate(() => {
      const probe = document.body.appendChild(document.createElement("div"));
      probe.style.backgroundColor = "var(--bg-selected)";
      const color = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return color;
    }),
  });
  assert.ok((await centerOffset(timeline, "m40")) <= 4);
});

test("Needs you: Show message on a live arrival does not replay its arrival when the tint ends", async (t) => {
  const value = frame(1, "alpha");
  value.frame.messages = [
    message("old", "Old", { at_ms: Date.now() - 60_000 }),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.locator('[data-message-id="old"]').waitFor();
  const next = structuredClone(value);
  next.frame.messages.push(message("new", "New", { at_ms: Date.now() + 1000 }));
  next.frame.health = { level: "needs_you", items: 1 };
  next.frame.attention = [
    {
      source_kind: "delivery_attempt",
      summary: { text: "No reply.", truncated: false },
      opened_at_ms: Date.now(),
      subject_alias: "codex",
      message_id: "new",
    },
  ];
  ui.connections[0].send("space", next);
  const row = timeline.locator('[data-message-id="new"]');
  await ui.page.waitForFunction(
    () =>
      document
        .querySelector('.timeline [data-message-id="new"]')
        ?.getAnimations().length === 0,
  );
  await ui.page
    .getByRole("button", { name: "Show message from @codex", exact: true })
    .click();
  assert.equal(
    await row.evaluate((element) => element.classList.contains("located")),
    true,
  );
  await ui.page.waitForFunction(
    () =>
      !document
        .querySelector('.timeline [data-message-id="new"]')
        .classList.contains("located"),
  );
  assert.deepEqual(
    await row.evaluate((element) => [
      element.classList.contains("arrive"),
      element.getAnimations().map((animation) => animation.animationName),
    ]),
    [true, []],
  );
});

test("thread pane: a thread with no replies reads No replies yet, and one with unloaded replies keeps its count", async (t) => {
  const value = locatedChannel();
  value.frame.threads = [
    {
      thread_id: "m40",
      title: { text: "Request 40", truncated: false },
      message_count: 3,
    },
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const items = ui.page
    .getByRole("region", { name: "Attention needed" })
    .getByRole("listitem");
  const pane = ui.page.getByRole("complementary", { name: "Thread" });
  await items
    .nth(0)
    .getByRole("button", { name: "Show message from @codex", exact: true })
    .click();
  await pane.locator('[data-message-id="m20"]').waitFor();
  assert.equal(await pane.locator(".divider").textContent(), "No replies yet");
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  await items
    .nth(2)
    .getByRole("button", { name: "Show message", exact: true })
    .click();
  await pane.locator('[data-message-id="m40"]').waitFor();
  assert.equal(
    await pane.locator(".divider").textContent(),
    "0 replies · earlier replies not loaded",
  );
});

test("design-b: the layout gives the timeline the room, wraps members in two lines, and never scrolls sideways", async (t) => {
  const value = frame(1, "alpha", "needs_you");
  value.frame.attention = [
    {
      source_kind: "agent_status",
      summary: {
        text: "@claude is waiting on an approval or question.",
        truncated: false,
      },
      opened_at_ms: Date.now(),
    },
  ];
  value.frame.participants = Array.from({ length: 4 }, (_, n) => ({
    ...fixture.frame.participants[0],
    address_id: `agent-${n}`,
    alias: `@agent${n}`,
  }));
  const crowd = structuredClone(value);
  crowd.key = key(2);
  crowd.frame.space = {
    ...crowd.frame.space,
    key: key(2),
    display_name: "crowd",
  };
  crowd.frame.health = { level: "calm" };
  crowd.frame.participants = Array.from({ length: 40 }, (_, n) => ({
    ...fixture.frame.participants[0],
    address_id: `crowd-${n}`,
    alias: `@crowded-agent-${n}`,
  }));
  const exchanged = exchange();
  exchanged.key = key(3);
  exchanged.frame.space = {
    ...exchanged.frame.space,
    key: key(3),
    display_name: "talk",
  };
  const ui = await open(t, (client) =>
    replay(client, [value, crowd, exchanged]),
  );
  await ui.page.setViewportSize({ width: 1440, height: 900 });
  await ui.live();
  const share = await ui.page
    .locator(".timeline")
    .evaluate(
      (element) => element.getBoundingClientRect().height / innerHeight,
    );
  assert.ok(share >= 0.7, `timeline holds ${Math.round(share * 100)}%`);
  await ui.page.setViewportSize({ width: 1024, height: 700 });
  await ui.row(2).click();
  const more = ui.page.getByRole("button", { name: /^\+\d+ more$/ });
  await more.waitFor();
  const strip = await ui.page
    .locator(".members")
    .evaluate((element) => element.getBoundingClientRect().height);
  assert.ok(strip <= 2 * 36 + 10 + 2, `member strip is ${strip}px tall`);
  const hidden = Number((await more.textContent()).match(/\d+/)[0]);
  const shown = await ui.page.locator(".members li[data-address-id]").count();
  assert.equal(shown + hidden, 40);
  await more.click();
  assert.equal(
    await ui.page.locator(".members li[data-address-id]").count(),
    40,
  );
  await ui.row(3).click();
  await ui.page.getByRole("button", { name: /^1 reply from/ }).click();
  await ui.page.getByRole("complementary", { name: "Thread" }).waitFor();
  assert.equal(
    await ui.page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
});

function team(n, name) {
  const value = frame(n, name);
  value.frame.participants = [
    ...people(),
    {
      ...people()[0],
      address_id: "claude",
      alias: "@claude",
      kind: { type: "agent", runtime: "claude-code" },
      activity: "working",
    },
  ];
  return value;
}
async function until(check, what) {
  const deadline = Date.now() + 5000;
  while (!check()) {
    if (Date.now() > deadline) assert.fail(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
function requestIds(ui) {
  return ui.posts.attempts.map((attempt) => JSON.parse(attempt.raw).request_id);
}
async function activeOption(page, box) {
  const id = await box.getAttribute("aria-activedescendant");
  return page.locator(`[id="${id}"] .opt-name`).textContent();
}
const measures = (page, name) =>
  page.evaluate(
    (name) =>
      performance
        .getEntriesByName(name, "measure")
        .map((entry) => entry.duration),
    name,
  );

test("UI-3: the # and @ popup moves with Up and Down, accepts with Tab and Enter, and closes with Escape", async (t) => {
  const beta = team(2, "beta");
  beta.frame.participants = [
    ...beta.frame.participants.filter(({ alias }) => alias !== "@claude"),
    {
      ...people()[0],
      address_id: "coder",
      alias: "@coder",
      activity: "waiting",
    },
  ];
  const ui = await open(t, (client) =>
    replay(client, [team(1, "alpha"), beta]),
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("#");
  const channels = ui.page.getByRole("listbox", { name: "Channels" });
  await channels.waitFor();
  assert.deepEqual(await channels.locator(".opt-name").allTextContents(), [
    "alpha",
    "beta",
    "#all",
  ]);
  assert.equal(
    await channels.getByRole("option").last().textContent(),
    "#all· every workspace",
  );
  assert.equal(await box.getAttribute("aria-expanded"), "true");
  assert.equal(await activeOption(ui.page, box), "alpha");
  await box.press("ArrowDown");
  assert.equal(await activeOption(ui.page, box), "beta");
  await box.press("ArrowUp");
  await box.press("ArrowUp");
  assert.equal(await activeOption(ui.page, box), "#all", "Up wraps to the end");
  await box.press("ArrowDown");
  assert.equal(await activeOption(ui.page, box), "alpha", "Down wraps");
  assert.equal(
    await channels.locator('[aria-selected="true"] .opt-name').textContent(),
    "alpha",
  );
  await box.press("Escape");
  await channels.waitFor({ state: "detached" });
  assert.equal(await box.inputValue(), "#");
  assert.equal(await box.getAttribute("aria-expanded"), "false");
  await box.pressSequentially("b");
  await channels.waitFor();
  await box.press("Enter");
  assert.equal(await box.inputValue(), "#beta ");
  await box.pressSequentially("@");
  const members = ui.page.getByRole("listbox", { name: "Members" });
  await members.waitFor();
  assert.deepEqual(
    await members.locator(".opt-name").allTextContents(),
    ["@coder", "@codex", "@shell"],
    "the members of #beta, not of the open channel",
  );
  assert.deepEqual(await members.locator(".opt-presence").allTextContents(), [
    "Waiting",
    "Idle",
    "Idle",
  ]);
  await box.press("ArrowDown");
  await box.press("Tab");
  assert.equal(await box.inputValue(), "#beta @codex ");
  assert.equal(
    await box.evaluate((element) => document.activeElement === element),
    true,
    "Tab accepted instead of moving focus",
  );
  await box.pressSequentially("#");
  await channels.waitFor();
  await channels.getByRole("option").filter({ hasText: "alpha" }).click();
  assert.equal(await box.inputValue(), "#beta @codex #alpha ");
  assert.equal(ui.posts.attempts.length, 0, "accepting never sends");
  const popupTimes = await measures(ui.page, "namedrop:keystroke-to-popup");
  assert.ok(popupTimes.length >= 4, `${popupTimes.length} popup measures`);
  assert.ok(popupTimes.every((duration) => duration >= 0 && duration < 1000));
});

test("UI-3: typing #al, Tab, @co, Tab gives #alpha @codex and Enter posts it", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [team(1, "alpha"), team(2, "beta")]),
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("#al");
  await ui.page.getByRole("listbox", { name: "Channels" }).waitFor();
  await box.press("Tab");
  await box.pressSequentially("@co");
  await ui.page.getByRole("listbox", { name: "Members" }).waitFor();
  await box.press("Tab");
  assert.equal(await box.inputValue(), "#alpha @codex ");
  await box.pressSequentially("say hi");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the post");
  const [attempt] = ui.posts.attempts;
  assert.equal(attempt.key, key(1));
  assert.equal(attempt.type, "application/json");
  assert.equal(attempt.csrf, "csrf-1");
  assert.deepEqual(Object.keys(JSON.parse(attempt.raw)), [
    "request_id",
    "source",
  ]);
  assert.equal(JSON.parse(attempt.raw).source, "#alpha @codex say hi");
});

test("UI-3: a post shows Sending…, then Sent, and its real row replaces the pending row in place", async (t) => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const value = team(1, "alpha");
  value.frame.messages = [
    message("earlier", "Earlier from codex", {
      author: "codex",
      at_ms: Date.now() - 60_000,
    }),
  ];
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    {
      onPost: async () => {
        await gate;
        return { status: 200 };
      },
    },
  );
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@codex say hi");
  await box.press("Enter");
  const pending = timeline.locator("article.pending");
  await pending.getByText("Sending…", { exact: true }).waitFor();
  assert.equal(await pending.locator(".body").textContent(), "@codex say hi");
  assert.equal(
    await pending.evaluate((row) => getComputedStyle(row).opacity),
    "0.6",
  );
  assert.equal(
    await box.inputValue(),
    "@codex say hi",
    "the box keeps its text",
  );
  assert.equal(await box.getAttribute("readonly"), "");
  assert.equal(
    await ui.page.locator(".composer-status").textContent(),
    "Sending…",
  );
  release();
  await pending.getByText("Sent", { exact: true }).waitFor();
  assert.equal(await box.inputValue(), "");
  assert.equal(await box.getAttribute("readonly"), null);
  assert.equal((await measures(ui.page, "namedrop:enter-to-sent")).length, 1);
  const before = await pending.boundingBox();
  const update = structuredClone(value);
  update.frame.messages.push(ui.posts.postedRow(requestIds(ui)[0]));
  ui.connections[0].send("space", update);
  const real = timeline.locator('[data-message-id="posted-1"]');
  await real.waitFor();
  assert.equal(await pending.count(), 0);
  assert.equal(
    await timeline.getByText("@codex say hi", { exact: true }).count(),
    1,
  );
  assert.equal(
    await real.evaluate((row) => row.classList.contains("arrive")),
    false,
  );
  const after = await real.boundingBox();
  assert.ok(
    Math.abs(after.y - before.y) <= 1,
    `the row stayed in place: ${before.y} then ${after.y}`,
  );
});

test("UI-3: pressing Enter twice writes one post", async (t) => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const value = team(1, "alpha");
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    {
      onPost: async () => {
        await gate;
        return { status: 200 };
      },
    },
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@codex once");
  await box.press("Enter");
  await box.press("Enter");
  await box.pressSequentially("ignored while sending");
  assert.equal(await box.inputValue(), "@codex once");
  release();
  await ui.page.getByText("Sent", { exact: true }).waitFor();
  await box.pressSequentially("@codex twice in one task");
  await box.evaluate((element) => {
    for (let n = 0; n < 2; n++)
      element.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        }),
      );
  });
  await until(() => ui.posts.receipts.size === 2, "both posts");
  assert.equal(ui.posts.attempts.length, 2);
  assert.equal(new Set(requestIds(ui)).size, 2);
  const update = structuredClone(value);
  update.frame.messages = requestIds(ui).map((id, n) =>
    ui.posts.postedRow(id, Date.now() + n),
  );
  ui.connections[0].send("space", update);
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.locator('[data-message-id="posted-2"]').waitFor();
  assert.equal(await timeline.locator("article.pending").count(), 0);
  assert.equal(await timeline.locator("[data-message-id]").count(), 2);
});

test("UI-3: a thread reply carries in_reply_to and keeps its recipients", async (t) => {
  const now = Date.now();
  const value = team(1, "alpha");
  value.frame.messages = [
    message("ask", "@codex check this", {
      thread_id: "t1",
      at_ms: now - 120_000,
      legs: [{ to: "codex", stage: done() }],
    }),
    message("answer", "Checked", {
      thread_id: "t1",
      in_reply_to: "ask",
      kind: "reply",
      author: "codex",
      at_ms: now - 60_000,
    }),
  ];
  value.frame.threads = [
    {
      thread_id: "t1",
      title: { text: "Check", truncated: false },
      message_count: 2,
    },
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.getByRole("button", { name: /^1 reply from @codex/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  assert.equal(await pane.locator(".composer-to").textContent(), "To @codex");
  await reply.click();
  await reply.pressSequentially("thanks");
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the reply");
  const first = JSON.parse(ui.posts.attempts[0].raw);
  assert.equal(first.source, "@codex thanks");
  assert.equal(first.in_reply_to, "answer");
  await pane.locator("article.pending").getByText("Sent").waitFor();
  assert.equal(await timeline.locator("article.pending").count(), 0);
  await reply.pressSequentially("@claude look too");
  assert.equal(await pane.locator(".composer-to").count(), 0);
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the second reply");
  const second = JSON.parse(ui.posts.attempts[1].raw);
  assert.equal(second.source, "@claude look too");
  assert.equal(second.in_reply_to, "answer");
});

test("UI-3: a thread reply to an agent in another workspace names it through its home channel", async (t) => {
  const now = Date.now();
  const testbed = team(1, "testbed");
  testbed.frame.space.addressable_label = null;
  testbed.frame.messages = [
    message("ask", "#alpha @lead please reply", {
      thread_id: "t1",
      at_ms: now - 120_000,
      legs: [{ to: "envoy:w2:pane-9", stage: done() }],
    }),
    message("answer", "Replying from alpha", {
      thread_id: "t1",
      in_reply_to: "ask",
      kind: "reply",
      author: "envoy:w2:pane-9",
      at_ms: now - 60_000,
    }),
  ];
  testbed.frame.threads = [
    {
      thread_id: "t1",
      title: { text: "Please reply", truncated: false },
      message_count: 2,
    },
  ];
  const alpha = frame(2, "alpha");
  alpha.frame.participants = [
    { ...people()[0], address_id: "pane-9", alias: "@lead" },
  ];
  const ui = await open(t, (client) => replay(client, [testbed, alpha]));
  await ui.live();
  await ui.row(1).click();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.getByRole("button", { name: /^1 reply from @lead/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox");
  await reply.waitFor();
  assert.deepEqual(await pane.locator(".composer-to").allTextContents(), [
    "To @lead · #alpha",
  ]);
  assert.equal(
    await reply.getAttribute("aria-label"),
    "Reply to @lead · #alpha",
  );
  await reply.click();
  await reply.pressSequentially("thanks");
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the reply");
  const [attempt] = ui.posts.attempts;
  assert.equal(attempt.key, key(1));
  const body = JSON.parse(attempt.raw);
  assert.equal(body.source, "#alpha @lead thanks");
  assert.equal(body.in_reply_to, "answer");
});

test("UI-3: a post that names nobody stays in the box and says how to choose someone", async (t) => {
  const ui = await open(t, (client) => replay(client, [team(1, "alpha")]));
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  const send = ui.page.getByRole("button", { name: "Send" });
  const status = ui.page.locator(".composer-status");
  await box.click();
  assert.equal(
    await status.textContent(),
    "Enter to send · Shift+Enter for a new line",
  );
  await box.pressSequentially("hello");
  await box.press("Enter");
  assert.equal(
    await status.textContent(),
    "Start with @name to choose who gets this.",
  );
  assert.equal(await send.isDisabled(), true);
  assert.equal(await box.inputValue(), "hello");
  assert.equal(await ui.page.locator("article.pending").count(), 0);
  assert.equal(
    await status
      .locator("p")
      .evaluate((element) => getComputedStyle(element).color),
    await box.evaluate((element) => getComputedStyle(element).color),
    "the guard reads in --text, not muted",
  );
  await box.fill("@codex hello");
  assert.equal(await send.isDisabled(), false);
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the addressed post");
  assert.equal(
    JSON.parse(ui.posts.attempts[0].raw).source,
    "@codex hello",
    "the unaddressed text never reached the server",
  );
});

test("UI-3: a 401 reads as an expired session", async (t) => {
  const ui = await open(
    t,
    (client) => replay(client, [team(1, "alpha")]),
    () => [],
    { onPost: () => ({ status: 401, error: "No session." }) },
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@codex hi");
  await box.press("Enter");
  const error = ui.page.locator(".composer").getByRole("alert");
  await error.waitFor();
  assert.equal(
    await error.textContent(),
    "Your session expired. Reload the page to keep posting.",
  );
});

test("UI-3: a refused post keeps the text, shows the server's message, and retries with the same id", async (t) => {
  const ui = await open(
    t,
    (client) => replay(client, [team(1, "alpha")]),
    () => [],
    { onPost: () => ({ status: 400, error: "unknown recipient @ghost" }) },
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@ghost hi");
  await box.press("Enter");
  const error = ui.page.locator(".composer").getByRole("alert");
  await error.waitFor();
  assert.equal(
    await error.textContent(),
    "Unknown recipient @ghost.",
    "the server's words as a sentence",
  );
  assert.equal(await box.inputValue(), "@ghost hi");
  assert.equal(await box.getAttribute("readonly"), null);
  assert.equal(await ui.page.locator("article.pending").count(), 0);
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the retry");
  await error.waitFor();
  assert.equal(new Set(requestIds(ui)).size, 1, "the retry reused its id");
  await box.pressSequentially("!");
  await error.waitFor({ state: "detached" });
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 3, "the edited post");
  assert.equal(new Set(requestIds(ui)).size, 2, "changed text gets a new id");
});

test("UI-3: a 403 refreshes the CSRF token once and retries the same post", async (t) => {
  const ui = await open(t, (client) => replay(client, [team(1, "alpha")]));
  await ui.live();
  await until(() => ui.posts.session.tokenReads === 1, "the startup token");
  ui.posts.rotate();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@codex hi");
  await box.press("Enter");
  await ui.page.locator("article.pending").getByText("Sent").waitFor();
  assert.deepEqual(
    ui.posts.attempts.map(({ csrf, status }) => [csrf, status]),
    [
      ["csrf-1", 403],
      ["csrf-2", 200],
    ],
  );
  assert.equal(new Set(requestIds(ui)).size, 1);
  assert.equal(ui.posts.session.tokenReads, 2);
});

test("UI-3: channels that share a label are listed disabled with a tooltip and cannot be chosen", async (t) => {
  const twin = (n) => {
    const value = frame(n, "twin");
    value.frame.space.addressable_label = null;
    return value;
  };
  const ui = await open(t, (client) =>
    replay(client, [team(1, "alpha"), twin(2), twin(3)]),
  );
  await ui.live();
  await ui.row(1).click();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("#tw");
  const list = ui.page.getByRole("listbox", { name: "Channels" });
  await list.waitFor();
  const options = list.getByRole("option");
  assert.equal(await options.count(), 2);
  for (const option of await options.all()) {
    assert.equal(await option.getAttribute("aria-disabled"), "true");
    assert.equal(
      await option.getAttribute("title"),
      "Two workspaces share this label. Rename one in Herdr to mention it.",
    );
    assert.equal(
      await option.evaluate((element) => getComputedStyle(element).opacity),
      "0.5",
    );
  }
  await box.press("Enter");
  await box.press("Tab");
  // Playwright refuses to click an aria-disabled element unless forced, unlike a person.
  await options.first().click({ force: true });
  assert.equal(await box.inputValue(), "#tw");
  assert.equal(ui.posts.attempts.length, 0);
});

test("UI-3: headers show two recipients then +N, envoys read as words, and a paused channel holds queued legs", async (t) => {
  const now = Date.now();
  const alpha = team(1, "alpha");
  alpha.frame.health = { level: "paused" };
  const queued = { stage: "queued" };
  alpha.frame.messages = [
    message("wide", "Everyone look", {
      at_ms: now - 50_000,
      legs: ["codex", "shell", "claude"].map((to) => ({ to, stage: queued })),
    }),
    message("remote", "From beta", {
      author: "envoy:w2:pane-9",
      at_ms: now - 40_000,
    }),
    message("lost", "Lost agent", {
      author: "envoy:w2:pane-0",
      at_ms: now - 30_000,
    }),
    message("far", "Far agent", {
      author: "envoy:w9:pane-9",
      at_ms: now - 20_000,
    }),
    message("odd", "Odd id", { author: "stranger", at_ms: now - 10_000 }),
  ];
  const beta = frame(2, "beta");
  beta.frame.participants = [
    { ...people()[0], address_id: "pane-9", alias: "@reviewer" },
  ];
  const ui = await open(t, (client) => replay(client, [alpha, beta]));
  await ui.live();
  await ui.row(1).click();
  const row = (id) => ui.page.locator(`[data-message-id="${id}"]`);
  const to = row("wide").locator(".to");
  assert.equal(await to.textContent(), " to @codex, @shell +1");
  assert.equal(await to.getAttribute("title"), "@codex, @shell, @claude");
  assert.equal(
    await row("wide").locator(".receipt").textContent(),
    "@codexHeld while paused@shellHeld while paused@claudeHeld while paused",
  );
  assert.equal(
    await row("remote").locator(".author").textContent(),
    "@reviewer · #beta",
  );
  assert.equal(
    await row("lost").locator(".author").textContent(),
    "an agent in #beta",
  );
  assert.equal(
    await row("far").locator(".author").textContent(),
    "an agent in another workspace",
  );
  const odd = row("odd");
  assert.equal(await odd.locator(".author").textContent(), "an unknown agent");
  assert.equal(
    await odd.locator(".author [title]").getAttribute("title"),
    "stranger",
  );
  assert.equal(await odd.locator(".avatar").textContent(), "?");
  assert.ok(
    await odd
      .locator(".avatar")
      .evaluate((avatar) => avatar.classList.contains("id-terminal")),
  );
});

test("UI-3: the box grows a line at a time up to eight lines, then scrolls, and shrinks when it clears", async (t) => {
  const ui = await open(t, (client) => replay(client, [team(1, "alpha")]));
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  const height = () =>
    box.evaluate((element) => element.getBoundingClientRect().height);
  await box.click();
  assert.equal(await height(), 32, "one line");
  await box.pressSequentially("@codex line 1");
  await box.press("Shift+Enter");
  await box.pressSequentially("line 2");
  assert.equal(await height(), 32 + 21, "two lines");
  for (let line = 3; line <= 10; line++) {
    await box.press("Shift+Enter");
    await box.pressSequentially(`line ${line}`);
  }
  assert.equal(await height(), 32 + 7 * 21, "eight lines at most");
  assert.ok(
    await box.evaluate(
      (element) => element.scrollHeight > element.clientHeight,
    ),
    "the ninth and tenth lines scroll",
  );
  await box.press("Enter");
  await ui.page.locator("article.pending").getByText("Sent").waitFor();
  assert.equal(await height(), 32, "back to one line");
});
