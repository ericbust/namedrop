import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { chromium } from "playwright-core";
import { build } from "vite";
import { threadControlCopy } from "../src/copy.ts";

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
    not_addressable_reason: null,
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
  const controls = [];
  const deliveries = [];
  const receipts = new Map();
  const authorized = (req) =>
    (req.headers.cookie ?? "")
      .split(/;\s*/)
      .includes(`${COOKIE}=${session.secret}`);
  return {
    session,
    attempts,
    controls,
    deliveries,
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
    async control(req, res, onControl, path) {
      const request = {
        method: req.method,
        path,
        csrf: req.headers["x-namedrop-csrf"] ?? null,
        type: req.headers["content-type"] ?? null,
        body: await readBody(req),
        status: 0,
        closedUnanswered: false,
      };
      res.on("close", () => {
        request.closedUnanswered = !res.writableFinished;
      });
      controls.push(request);
      const reply = (status, value) => {
        request.status = status;
        sendJson(res, status, value);
      };
      if (!authorized(req)) return reply(401, { error: "No session." });
      if (request.csrf !== session.csrf) return reply(403, {});
      const { status = 200 } = await onControl(request);
      reply(status, status === 200 ? {} : { error: "Refused." });
    },
    async deliver(req, res, onDeliver, path) {
      const request = {
        method: req.method,
        path,
        csrf: req.headers["x-namedrop-csrf"] ?? null,
        type: req.headers["content-type"] ?? null,
        body: await readBody(req),
        status: 0,
      };
      deliveries.push(request);
      const reply = (status, value) => {
        request.status = status;
        sendJson(res, status, value);
      };
      if (!authorized(req)) return reply(401, { error: "No session." });
      if (request.csrf !== session.csrf) return reply(403, {});
      const asked = JSON.parse(request.body);
      const outcome = await onDeliver(asked, request);
      if (outcome.drop) return req.socket.destroy();
      if (outcome.raw !== undefined) {
        request.status = 200;
        res.writeHead(200, { "Content-Type": "application/json" });
        return res.end(outcome.raw);
      }
      if (outcome.stall) {
        request.status = 200;
        request.closed = false;
        req.socket.once("close", () => {
          request.closed = true;
        });
        res.writeHead(200, { "Content-Type": "application/json" });
        return res.write('{"delivered":');
      }
      const { status = 200, ...answer } = outcome;
      if (status === 200) return reply(200, { delivered: asked.to, ...answer });
      if (answer.error !== undefined)
        return reply(status, { error: answer.error });
      request.status = status;
      res.writeHead(status).end();
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
function screenRoute(onScreen) {
  const requests = [];
  let inFlight = 0;
  let maxInFlight = 0;
  return {
    requests,
    maxInFlight: () => maxInFlight,
    async serve(res, key, address) {
      requests.push({ key, address, at: Date.now() });
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      try {
        const {
          status = 200,
          body = "",
          delayMs = 0,
        } = await onScreen({ key, address });
        if (delayMs > 0)
          await new Promise((resolve) => setTimeout(resolve, delayMs));
        res.writeHead(status, {
          "Content-Type": "text/plain; charset=utf-8",
          "Cache-Control": "no-store",
        });
        res.end(body);
      } finally {
        inFlight -= 1;
      }
    },
  };
}
async function open(
  t,
  onConnect,
  history = () => [],
  {
    path = "/",
    onPost = () => ({ status: 200 }),
    onScreen = () => ({ body: "" }),
    onContext = null,
    onControl = () => ({ status: 200 }),
    onDeliver = () => ({ status: 200 }),
    observeLanding = false,
  } = {},
) {
  const clients = new Set();
  const connections = [];
  const errors = [];
  const historyRequests = [];
  const contextRequests = [];
  const posts = postRoute(onPost);
  const screens = screenRoute(onScreen);
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
    if (/^\/w\/[^/]+\/threads\/[^/]+\/(continue|stop)$/.test(url.pathname))
      return posts.control(req, res, onControl, url.pathname);
    if (
      req.method === "POST" &&
      /^\/w\/[^/]+\/messages\/[^/]+\/deliver$/.test(url.pathname)
    )
      return posts.deliver(req, res, onDeliver, url.pathname);
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
    if (/^\/w\/[^/]+\/messages\/[^/]+\/context$/.test(url.pathname)) {
      contextRequests.push(url);
      if (onContext === null) return res.writeHead(404).end();
      const context = await onContext(url);
      return context === null
        ? res.writeHead(404).end()
        : sendJson(res, 200, context);
    }
    const screen = /^\/w\/([^/]+)\/participants\/([^/]+)\/screen$/.exec(
      url.pathname,
    );
    if (req.method === "GET" && screen !== null)
      return screens.serve(res, screen[1], decodeURIComponent(screen[2]));
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
  await page.addInitScript((observeLanding) => {
    window.cspErrors = [];
    window.initialHistoryLength = history.length;
    document.addEventListener("securitypolicyviolation", (event) =>
      window.cspErrors.push(event.violatedDirective),
    );
    if (observeLanding) {
      window.landingHeadings = [];
      window.operatorInputs = [];
      for (const name of ["pointerdown", "keydown", "wheel", "touchstart"])
        addEventListener(name, () => window.operatorInputs.push(name), true);
      new MutationObserver(() => {
        const heading = document.querySelector("main h1:not(.sr-only)");
        if (
          heading?.getClientRects().length &&
          heading.textContent !== window.landingHeadings.at(-1)
        )
          window.landingHeadings.push(heading.textContent);
      }).observe(document, {
        childList: true,
        subtree: true,
        characterData: true,
      });
    }
  }, observeLanding);
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
  return {
    page,
    channels,
    row,
    connections,
    live,
    historyRequests,
    contextRequests,
    posts,
    screens,
  };
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
  await until(() => ui.connections.length > 0, "the first stream");
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
    [{ stage: "completed", kind: "settled" }, "Ended without an answer", null],
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
    message("command", "ls", {
      legs: [{ to: "shell", stage: { stage: "completed", kind: "settled" } }],
    }),
  );
  value.frame.participants = people();
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
    ["an unknown agentEnded without an answer", "YouWorking on it"],
  );
  assert.equal(
    await ui.page
      .locator('[data-message-id="command"] .receipt li')
      .textContent(),
    "Done",
    "a terminal's settled command is done",
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
test("landing without input: ready opens the channel that needs you first", async (t) => {
  const needsYou = attentionFrame("agent_status");
  needsYou.key = key(3);
  needsYou.frame.space = frame(3, "gamma").frame.space;
  needsYou.frame.messages = [message("gamma-request", "Please check gamma")];
  const alpha = frame(1, "alpha");
  alpha.frame.messages = [
    message("alpha-recent", "Recent calm message", { at_ms: 500 }),
  ];
  const ui = await open(
    t,
    (client) => replay(client, [alpha, needsYou]),
    undefined,
    { observeLanding: true },
  );
  await ui.live();
  assert.equal(
    await ui.page.getByRole("heading", { level: 1 }).textContent(),
    "gamma",
  );
  assert.equal(await ui.row(3).getAttribute("aria-current"), "page");
  assert.equal(await ui.row(1).getAttribute("aria-current"), null);
  const shortcut = ui.page.getByRole("navigation", {
    name: "Needs you",
    exact: true,
  });
  assert.equal(await shortcut.getByRole("button").count(), 1);
  assert.equal(await shortcut.getByRole("button").isVisible(), true);
  assert.match(
    await shortcut.textContent(),
    /gamma.*Waiting on approval or a question/,
  );
  const banner = ui.page.getByRole("region", { name: "Attention needed" });
  assert.equal(await banner.isVisible(), true);
  await banner
    .getByText("Waiting on approval or a question", { exact: true })
    .waitFor();
  await ui.page.getByText("Please check gamma", { exact: true }).waitFor();
  assert.deepEqual(await ui.page.evaluate(() => window.landingHeadings), [
    "gamma",
  ]);
  assert.deepEqual(await ui.page.evaluate(() => window.operatorInputs), []);
  assert.equal(await ui.page.evaluate(() => location.pathname), "/");
  assert.equal(ui.posts.attempts.length, 0);
});

test("landing without input: late ready keeps alpha visible while Needs you lists late gamma", async (t) => {
  const alpha = frame(1, "alpha");
  alpha.frame.messages = [message("alpha-message", "Alpha is calm")];
  const ui = await open(t, (client) => client.send("space", alpha), undefined, {
    observeLanding: true,
  });
  await ui.live();
  await ui.page.getByRole("heading", { name: "alpha", exact: true }).waitFor();
  await ui.page.waitForTimeout(400);
  assert.deepEqual(await ui.page.evaluate(() => window.landingHeadings), [
    "alpha",
  ]);
  assert.equal(
    await ui.page
      .getByRole("navigation", { name: "Needs you", exact: true })
      .count(),
    0,
  );
  const needsYou = attentionFrame("agent_status");
  needsYou.key = key(3);
  needsYou.frame.space = frame(3, "gamma").frame.space;
  needsYou.frame.messages = [message("gamma-request", "Please check gamma")];
  ui.connections[0].send("space", needsYou);
  ui.connections[0].send("ready", { keys: [key(1), key(3)] });
  const shortcut = ui.page.getByRole("navigation", {
    name: "Needs you",
    exact: true,
  });
  await shortcut.getByRole("button").waitFor();
  await nextFrames(ui.page);
  assert.equal(await shortcut.getByRole("button").count(), 1);
  assert.match(
    await shortcut.textContent(),
    /gamma.*Waiting on approval or a question/,
  );
  assert.equal(
    await ui.page.getByRole("heading", { level: 1 }).textContent(),
    "alpha",
  );
  assert.equal(await ui.row(1).getAttribute("aria-current"), "page");
  assert.equal(await ui.row(3).getAttribute("aria-current"), null);
  assert.equal(
    await ui.page.getByRole("region", { name: "Attention needed" }).count(),
    0,
  );
  assert.equal(
    await ui.page.getByText("Alpha is calm", { exact: true }).isVisible(),
    true,
  );
  assert.equal(
    await ui.page.getByText("Please check gamma", { exact: true }).count(),
    0,
  );
  assert.deepEqual(await ui.page.evaluate(() => window.landingHeadings), [
    "alpha",
  ]);
  assert.deepEqual(await ui.page.evaluate(() => window.operatorInputs), []);
  assert.equal(await ui.page.evaluate(() => location.pathname), "/");
  assert.equal(ui.posts.attempts.length, 0);
  assert.equal(ui.connections.length, 1);
});

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

test("DESIGN-R2 B2: the reply count is never covered: the preview text takes the room beside it, and with under 120 px left the row shows the count only", async (t) => {
  const { value } = conversation();
  const latest = value.frame.messages.find((row) => row.message_id === "r2");
  latest.body.text =
    "Fixed: the CI cache was stale. I cleared it and the build passes now on every runner.";
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  const preview = ui.page.locator('[data-message-id="ask"] .reply-preview');
  const facts = async () => {
    await preview.scrollIntoViewIfNeeded();
    return preview.evaluate((button) => {
      const count = button.querySelector(".rp-count");
      const c = count.getBoundingClientRect();
      const hit = document.elementFromPoint(
        c.left + c.width / 2,
        c.top + c.height / 2,
      );
      const text = [...button.querySelectorAll(".rp-main, .rp-main *")]
        .map((element) => element.getBoundingClientRect())
        .filter((r) => r.width > 0 && r.height > 0);
      const main = button.querySelector(".rp-main")?.getBoundingClientRect();
      return {
        text: text.length > 0 ? Math.round(main.width) >= 120 : "count only",
        covered: text.some(
          (r) =>
            r.left < c.right &&
            r.right > c.left &&
            r.top < c.bottom &&
            r.bottom > c.top,
        ),
        countShown: count.contains(hit) && count.textContent === "2 replies",
        countFits: count.scrollWidth <= count.clientWidth,
      };
    });
  };
  for (const [width, height, text] of [
    [390, 844, "count only"],
    [599, 844, true],
    [640, 800, true],
    [1440, 900, true],
  ])
    assert.deepEqual(
      await (async () => {
        await ui.page.setViewportSize({ width, height });
        return facts();
      })(),
      { text, covered: false, countShown: true, countFits: true },
      `${width}x${height}`,
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
      "Post here to talk with the agents in this workspace. Mention @name to bring one in.",
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
async function eventually(check, what) {
  const deadline = Date.now() + 5000;
  while (!(await check())) {
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

test("UI-3: a thread reply carries in_reply_to and goes as typed to the thread's agents", async (t) => {
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
  assert.equal(first.source, "thanks", "the server delivers it to @codex");
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
  testbed.frame.space.not_addressable_reason = "unavailable";
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
  assert.equal(body.source, "thanks", "the server delivers it to @lead");
  assert.equal(body.in_reply_to, "answer");
});

// A thread whose only other member is a terminal, so a reply must name someone.
async function terminalThread(t) {
  const now = Date.now();
  const value = team(1, "alpha");
  value.frame.messages = [
    message("ls", "@shell ls", {
      thread_id: "t1",
      at_ms: now - 120_000,
      legs: [{ to: "shell", stage: { stage: "replied", reply_id: "out" } }],
    }),
    message(
      "out",
      "@shell command finished with exit code 0.\nstdout:\nREADME.md\n\nstderr:",
      {
        thread_id: "t1",
        in_reply_to: "ls",
        kind: "reply",
        author: "shell",
        at_ms: now - 60_000,
      },
    ),
  ];
  const ui = await open(t, (client) => replay(client, [value]));
  await ui.live();
  await ui.page
    .getByRole("region", { name: "Message history" })
    .getByRole("button", { name: /^1 reply from @shell/ })
    .click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  return {
    ui,
    pane,
    box: pane.getByRole("combobox", {
      name: "Start with @name to choose who gets this.",
    }),
    send: pane.getByRole("button", { name: "Send" }),
    status: pane.locator(".composer-status"),
  };
}

test("UI-3: a reply in a thread with no agents stays in the box and says how to choose someone", async (t) => {
  const { ui, pane, box, send, status } = await terminalThread(t);
  await box.click();
  assert.equal(
    await status.textContent(),
    "Enter to send · Shift+Enter for a new line",
  );
  assert.equal(await pane.locator(".composer-to").count(), 0, "no To label");
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
  await box.fill("@shell hello");
  assert.equal(await send.isDisabled(), false);
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the addressed post");
  const posted = JSON.parse(ui.posts.attempts[0].raw);
  assert.equal(
    posted.source,
    "@shell hello",
    "the unaddressed text never reached the server",
  );
  assert.equal(posted.in_reply_to, "out");
});

test("UI-3: the recipient guard waits for a send attempt and the next keystroke clears it", async (t) => {
  const { ui, box, send, status } = await terminalThread(t);
  const hint = "Enter to send · Shift+Enter for a new line";
  async function typeExpectingHint(text) {
    for (const character of text) {
      await box.pressSequentially(character);
      assert.equal(
        await status.textContent(),
        hint,
        `the hint with ${JSON.stringify(await box.inputValue())} in the box`,
      );
    }
  }
  await box.click();
  await typeExpectingHint("#alpha @");
  await ui.page.getByRole("listbox", { name: "Members" }).waitFor();
  assert.equal(await status.textContent(), hint, "the hint beside the popup");
  await box.fill("");
  await typeExpectingHint("hello");
  assert.equal(await send.isDisabled(), true);
  await send.click({ force: true });
  assert.equal(ui.posts.attempts.length, 0, "a disabled Send sends nothing");
  await box.press("Enter");
  assert.equal(
    await status.textContent(),
    "Start with @name to choose who gets this.",
  );
  assert.equal(await box.inputValue(), "hello");
  assert.equal(await ui.page.locator("article.pending").count(), 0);
  await typeExpectingHint("!");
  await box.fill("@shell hello");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the addressed post");
  assert.equal(
    JSON.parse(ui.posts.attempts[0].raw).source,
    "@shell hello",
    "neither the click nor the unaddressed Enter reached the server",
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

test("UI-3 fix2: a retry after an edit round trip, or after clearing and retyping, reuses the request id", async (t) => {
  let busy = true;
  const ui = await open(
    t,
    (client) => replay(client, [team(1, "alpha")]),
    () => [],
    { onPost: () => (busy ? { status: 503, error: "busy" } : { status: 200 }) },
  );
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@codex same text");
  await box.press("Enter");
  const error = ui.page.locator(".composer").getByRole("alert");
  await error.waitFor();
  await box.press("End");
  await box.press("x");
  await error.waitFor({ state: "detached" });
  await box.press("Backspace");
  assert.equal(await box.inputValue(), "@codex same text");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the round-trip retry");
  await error.waitFor();
  await box.press("Control+a");
  await box.press("Backspace");
  await error.waitFor({ state: "detached" });
  assert.equal(await box.inputValue(), "");
  await box.pressSequentially("@codex same text");
  busy = false;
  await box.press("Enter");
  await ui.page.locator("article.pending").getByText("Sent").waitFor();
  assert.deepEqual(
    ui.posts.attempts.map(({ status }) => status),
    [503, 503, 200],
  );
  assert.equal(
    new Set(requestIds(ui)).size,
    1,
    "every attempt carried the first id",
  );
});

test("UI-3 fix2: refused drafts in two channels and a thread each retry with their own request id", async (t) => {
  const ui = await open(
    t,
    (client) => replay(client, [askThread(), team(3, "gamma")]),
    () => [],
    { onPost: () => ({ status: 503, error: "busy" }) },
  );
  await ui.live();
  const error = ui.page.locator("main .composer").getByRole("alert");
  const roundTrip = async (box) => {
    await box.press("End");
    await box.press("x");
    await box.press("Backspace");
  };
  await ui.row(1).click();
  const alpha = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await alpha.click();
  await alpha.pressSequentially("@codex same");
  await alpha.press("Enter");
  await error.waitFor();
  await ui.row(3).click();
  const gamma = ui.page.getByRole("combobox", { name: "Message #gamma" });
  await gamma.click();
  await gamma.pressSequentially("@codex same");
  await gamma.press("Enter");
  await error.waitFor();
  await ui.row(1).click();
  await ui.page
    .getByRole("region", { name: "Message history" })
    .getByRole("button", { name: /^1 reply from @codex/ })
    .click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  await reply.click();
  await reply.pressSequentially("@codex same");
  await reply.press("Enter");
  await pane.locator(".composer").getByRole("alert").waitFor();
  assert.equal(new Set(requestIds(ui)).size, 3, "three drafts, three ids");
  await roundTrip(reply);
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 4, "the thread retry");
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  await alpha.click();
  await roundTrip(alpha);
  await alpha.press("Enter");
  await until(() => ui.posts.attempts.length === 5, "alpha's retry");
  await ui.row(3).click();
  await gamma.click();
  await roundTrip(gamma);
  await gamma.press("Enter");
  await until(() => ui.posts.attempts.length === 6, "gamma's retry");
  const ids = requestIds(ui);
  assert.deepEqual(
    ids.slice(3),
    [ids[2], ids[0], ids[1]],
    "each retry carried its own draft's id",
  );
  assert.equal(new Set(ids).size, 3);
});

test("UI-3 fix3: a refused thread reply retried after a new reply arrives posts to the new reply with a fresh request id", async (t) => {
  const value = askThread();
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    { onPost: () => ({ status: 503, error: "busy" }) },
  );
  await ui.live();
  await ui.page
    .getByRole("region", { name: "Message history" })
    .getByRole("button", { name: /^1 reply from @codex/ })
    .click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  await reply.click();
  await reply.pressSequentially("@codex same");
  await reply.press("Enter");
  await pane.locator(".composer").getByRole("alert").waitFor();
  const update = structuredClone(value);
  update.frame.messages.push(
    message("later", "One more thing", {
      thread_id: "t1",
      in_reply_to: "answer",
      kind: "reply",
      author: "codex",
      at_ms: Date.now(),
    }),
  );
  update.frame.threads[0].message_count = 3;
  ui.connections[0].send("space", update);
  await pane.locator('[data-message-id="later"]').waitFor();
  assert.equal(await reply.inputValue(), "@codex same");
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the retry");
  const [first, second] = ui.posts.attempts.map(({ raw }) => JSON.parse(raw));
  assert.equal(first.in_reply_to, "answer");
  assert.equal(second.in_reply_to, "later");
  assert.notEqual(
    second.request_id,
    first.request_id,
    "a new reply target gets a new id",
  );
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
    value.frame.space.not_addressable_reason = "shared_label";
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
      "Another workspace shows the same name. Give one a unique label in Herdr to mention it.",
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

test("UI-3: channels that share a label carry the sidebar's workspace suffix in the popup", async (t) => {
  const twin = (n) => {
    const value = frame(n, "twin");
    value.frame.space.addressable_label = null;
    value.frame.space.not_addressable_reason = "shared_label";
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
  assert.equal(await list.getByRole("option").count(), 2);
  const look = (locator) =>
    locator.evaluate((element) => {
      const style = getComputedStyle(element);
      return [style.color, style.fontSize];
    });
  for (const n of [2, 3]) {
    const suffix = `w${n} · ${key(n)}`;
    const option = list.getByRole("option", {
      name: `twin · ${suffix}`,
      exact: true,
    });
    assert.equal(await option.count(), 1, `the w${n} option names its suffix`);
    assert.equal(await option.getAttribute("aria-disabled"), "true");
    assert.deepEqual(
      await look(option.getByText(suffix)),
      await look(ui.row(n).getByText(suffix)),
      `the w${n} suffix looks like the sidebar's`,
    );
  }
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

test("UI-3: the box re-fits a wrapped placeholder when the viewport narrows and widens", async (t) => {
  const label = "release-coordination-and-incident-review";
  const ui = await open(t, (client) => replay(client, [team(1, label)]));
  await ui.live();
  const box = ui.page.getByRole("combobox", { name: `Message #${label}` });
  await ui.page.evaluate(() => {
    window.windowErrors = [];
    addEventListener("error", (event) =>
      window.windowErrors.push(event.message),
    );
  });
  const fit = async (width) => {
    await ui.page.setViewportSize({ width, height: 900 });
    return box.evaluate(
      (element) =>
        new Promise((resolve) =>
          requestAnimationFrame(() =>
            requestAnimationFrame(() =>
              resolve({
                height: element.getBoundingClientRect().height,
                scrollHeight: element.scrollHeight,
                clientHeight: element.clientHeight,
              }),
            ),
          ),
        ),
    );
  };
  const wide = await fit(1440);
  assert.equal(wide.height, 32, "one line when wide");
  const narrow = await fit(480);
  assert.ok(
    Math.abs(narrow.scrollHeight - narrow.clientHeight) <= 1,
    `the narrow box fits its placeholder: ${JSON.stringify(narrow)}`,
  );
  assert.ok(narrow.height > 32, "the placeholder wraps when narrow");
  assert.deepEqual(await fit(1440), wide, "back to one line when wide again");
  assert.deepEqual(await ui.page.evaluate(() => window.windowErrors), []);
});

function askThread() {
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
  return value;
}
// A post route that answers only once the test says so, always with the same 400.
function slowRefusal() {
  let refuse;
  const refused = new Promise((resolve) => {
    refuse = resolve;
  });
  return {
    onPost: () => refused,
    refuse: () => refuse({ status: 400, error: "unknown recipient @ghost" }),
  };
}
const notSent = (ui, n) =>
  ui.row(n).getByRole("img", { name: "A post wasn't sent" });

test("UI-3 fix: a post refused after switching channels comes back as that channel's draft with its error, and the sidebar marks it", async (t) => {
  const server = slowRefusal();
  const ui = await open(
    t,
    (client) => replay(client, [team(1, "alpha"), frame(3, "gamma")]),
    () => [],
    { onPost: server.onPost },
  );
  await ui.live();
  await ui.row(1).click();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("@ghost hi");
  await box.press("Enter");
  await ui.page.locator("main article.pending").waitFor();
  await ui.row(3).click();
  await ui.page.getByRole("heading", { name: "gamma", exact: true }).waitFor();
  const gammaBox = ui.page.getByRole("combobox", { name: "Message #gamma" });
  assert.equal(await gammaBox.inputValue(), "");
  assert.equal(await notSent(ui, 1).count(), 0, "no marker while sending");
  server.refuse();
  await notSent(ui, 1).waitFor();
  assert.equal(await notSent(ui, 3).count(), 0);
  assert.equal(
    await ui.page.locator(".composer").getByRole("alert").count(),
    0,
    "gamma's box shows no error",
  );
  assert.equal(await gammaBox.inputValue(), "");
  await ui.row(1).click();
  await box.waitFor();
  assert.equal(await box.inputValue(), "@ghost hi");
  const error = ui.page.locator(".composer").getByRole("alert");
  await error.waitFor();
  assert.equal(await error.textContent(), "Unknown recipient @ghost.");
  assert.equal(await box.getAttribute("readonly"), null);
  assert.equal(await ui.page.locator("article.pending").count(), 0);
  assert.equal(await notSent(ui, 1).count(), 1);
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the retry");
  await error.waitFor();
  assert.equal(new Set(requestIds(ui)).size, 1, "the retry reused its id");
  await box.pressSequentially("!");
  await error.waitFor({ state: "detached" });
  assert.equal(await notSent(ui, 1).count(), 0, "editing clears the marker");
});

test("UI-3 fix: a thread reply refused after the pane closed marks its parent, and opening the thread shows the draft and its error", async (t) => {
  const server = slowRefusal();
  const ui = await open(
    t,
    (client) => replay(client, [askThread()]),
    () => [],
    { onPost: server.onPost },
  );
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.getByRole("button", { name: /^1 reply from @codex/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  await reply.click();
  await reply.pressSequentially("thanks");
  await reply.press("Enter");
  await pane.locator("article.pending").waitFor();
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  const marker = timeline.getByRole("button", {
    name: "Reply not sent · Open thread",
  });
  assert.equal(await marker.count(), 0, "no marker while sending");
  server.refuse();
  await marker.waitFor();
  assert.equal(await marker.getAttribute("data-thread-id"), "t1");
  const parent = await timeline
    .locator('[data-message-id="ask"]')
    .boundingBox();
  const below = await marker.boundingBox();
  assert.ok(
    below.y >= parent.y + parent.height - 1,
    "the marker sits under its parent message",
  );
  assert.equal(await notSent(ui, 1).count(), 1);
  assert.equal(
    await ui.page.locator("main .composer").getByRole("alert").count(),
    0,
    "the channel's own box shows no error",
  );
  await marker.click();
  await pane.waitFor();
  assert.equal(await reply.inputValue(), "thanks");
  const error = pane.locator(".composer").getByRole("alert");
  await error.waitFor();
  assert.equal(await error.textContent(), "Unknown recipient @ghost.");
  assert.equal(
    await marker.count(),
    0,
    "the marker goes while the thread is open",
  );
  assert.equal(await pane.locator("article.pending").count(), 0);
  await reply.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the retry");
  assert.equal(new Set(requestIds(ui)).size, 1, "the retry reused its id");
  assert.equal(JSON.parse(ui.posts.attempts[1].raw).source, "thanks");
});

test("UI-3 fix: switching channels and closing a thread keep unsent text", async (t) => {
  const ui = await open(t, (client) =>
    replay(client, [askThread(), frame(3, "gamma")]),
  );
  await ui.live();
  await ui.row(1).click();
  const alpha = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await alpha.click();
  await alpha.pressSequentially("@codex half a post");
  await ui.row(3).click();
  const gamma = ui.page.getByRole("combobox", { name: "Message #gamma" });
  await gamma.waitFor();
  assert.equal(await gamma.inputValue(), "");
  await gamma.click();
  await gamma.pressSequentially("gamma draft");
  await ui.row(1).click();
  await alpha.waitFor();
  assert.equal(await alpha.inputValue(), "@codex half a post");
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  const preview = timeline.getByRole("button", {
    name: /^1 reply from @codex/,
  });
  await preview.click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  await reply.click();
  await reply.pressSequentially("half a reply");
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  await preview.click();
  await reply.waitFor();
  assert.equal(await reply.inputValue(), "half a reply");
  assert.equal(await alpha.inputValue(), "@codex half a post");
  await ui.row(3).click();
  await gamma.waitFor();
  assert.equal(await gamma.inputValue(), "gamma draft");
  assert.equal(ui.posts.attempts.length, 0);
});

test("UI-3 fix: the thread box says Write a reply while the To label names the recipients", async (t) => {
  const ui = await open(t, (client) => replay(client, [askThread()]));
  await ui.live();
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.getByRole("button", { name: /^1 reply from @codex/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  assert.equal(await pane.locator(".composer-to").textContent(), "To @codex");
  assert.equal(await reply.getAttribute("placeholder"), "Write a reply");
  assert.equal(
    await ui.page
      .getByRole("combobox", { name: "Message #alpha" })
      .getAttribute("placeholder"),
    "Message #alpha",
  );
});

test("UI-3 fix: a disabled channel's tooltip names a reason only when the page can be sure of it", async (t) => {
  const disabled = (n, name, reason = "unavailable", state = "active") => {
    const value = frame(n, name);
    value.frame.space.addressable_label = null;
    value.frame.space.not_addressable_reason = reason;
    value.frame.space.state = state;
    return value;
  };
  const ui = await open(t, (client) =>
    replay(client, [
      team(1, "alpha"),
      disabled(2, "w2", "missing_label"),
      disabled(3, "日本語", "empty_label"),
      disabled(4, "All", "reserved_all"),
      disabled(5, "Twin"),
      disabled(6, "twin"),
      disabled(7, "Dormant", "not_activated", "not_activated"),
      frame(8, "foobar"),
      disabled(9, "same", "shared_label"),
      disabled(10, "same", "shared_label"),
      disabled(11, "foobar"),
      disabled(12, "w13"),
      disabled(13, "w13", "missing_label"),
    ]),
  );
  await ui.live();
  await ui.row(1).click();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("#");
  const list = ui.page.getByRole("listbox", { name: "Channels" });
  await list.waitFor();
  const unsure =
    "Namedrop can't mention this workspace by its label. Check that the label is unique and isn't “all”, and that Namedrop is active there.";
  const noLabel =
    "This workspace has no label in Herdr. Name it to mention it.";
  const titles = {
    w2: noLabel,
    日本語:
      "This label has no Latin letters or digits, so Namedrop can't mention it. Rename it in Herdr.",
    All: unsure,
    Twin: unsure,
    twin: unsure,
    Dormant: unsure,
  };
  for (const [name, title] of Object.entries(titles)) {
    const option = list.getByRole("option").filter({
      has: ui.page.locator(".opt-name", { hasText: new RegExp(`^${name}$`) }),
    });
    assert.equal(await option.count(), 1, name);
    assert.equal(await option.getAttribute("aria-disabled"), "true", name);
    assert.equal(await option.getAttribute("title"), title, name);
  }
  const suffixed = [
    [/^foobar · w8 ·/, null],
    [/^foobar · w11 ·/, unsure],
    [/^w13 · w12 ·/, unsure],
    [/^w13 · w13 ·/, noLabel],
  ];
  for (const [name, title] of suffixed) {
    const option = list.getByRole("option").filter({
      has: ui.page.locator(".opt-name", { hasText: name }),
    });
    assert.equal(await option.count(), 1, String(name));
    assert.equal(
      await option.getAttribute("aria-disabled"),
      title === null ? null : "true",
      String(name),
    );
    assert.equal(await option.getAttribute("title"), title, String(name));
  }
  const same = list.getByRole("option").filter({
    has: ui.page.locator(".opt-name", { hasText: /^same ·/ }),
  });
  assert.equal(await same.count(), 2);
  for (const option of await same.all())
    assert.equal(
      await option.getAttribute("title"),
      "Another workspace shows the same name. Give one a unique label in Herdr to mention it.",
    );
  const alpha = list.getByRole("option").filter({
    has: ui.page.locator(".opt-name", { hasText: /^alpha$/ }),
  });
  assert.equal(await alpha.getAttribute("title"), null);
  assert.equal(await alpha.getAttribute("aria-disabled"), null);
});

test("UI-3 fix: # matches the typeable token ignoring case, and #Beta before @ lists beta's members", async (t) => {
  const project = frame(2, "Alpha Project");
  project.frame.space.addressable_label = "alpha-project";
  const beta = frame(3, "beta");
  beta.frame.participants = [
    { ...people()[0], address_id: "reviewer", alias: "@reviewer" },
  ];
  const ui = await open(t, (client) =>
    replay(client, [team(1, "alpha"), project, beta]),
  );
  await ui.live();
  await ui.row(1).click();
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.click();
  await box.pressSequentially("#alpha-p");
  const channels = ui.page.getByRole("listbox", { name: "Channels" });
  await channels.waitFor();
  assert.deepEqual(await channels.locator(".opt-name").allTextContents(), [
    "Alpha Project",
  ]);
  await box.press("Tab");
  assert.equal(await box.inputValue(), "#alpha-project ");
  await box.press("Control+a");
  await box.press("Backspace");
  await box.pressSequentially("#Beta @");
  const members = ui.page.getByRole("listbox", { name: "Members" });
  await members.waitFor();
  assert.deepEqual(await members.locator(".opt-name").allTextContents(), [
    "@reviewer",
  ]);
});

function crew(n, name, lanes, extra = {}) {
  const value = frame(n, name);
  const base = fixture.frame.participants[0];
  value.frame.participants = [
    {
      ...base,
      address_id: "human",
      alias: "you",
      kind: { type: "human" },
      activity: "idle",
    },
    ...lanes.map(([id, alias, activity, more = {}]) => ({
      ...base,
      address_id: id,
      alias,
      kind: { type: "agent", runtime: "codex" },
      activity,
      mailbox: [],
      waiting_on: [],
      ...more,
    })),
  ];
  Object.assign(value.frame, extra);
  return value;
}
function inbox(from, text, since) {
  return {
    delivery_id: `d-${since}`,
    message_id: `m-${since}`,
    from,
    preview: { text, truncated: false },
    state: "submitted",
    since_ms: since,
    attempts: 0,
  };
}
function busyTeam() {
  return [
    crew(1, "alpha", [
      [
        "codex",
        "@codex",
        "working",
        { mailbox: [inbox("@lead", "check the diff", 300)] },
      ],
      [
        "lead",
        "@lead",
        "waiting",
        { waiting_on: ["@codex"], mailbox: [inbox("you", "split it", 200)] },
      ],
    ]),
    crew(2, "beta", [["claude", "@claude", "blocked"]], {
      attention: [
        {
          source_kind: "agent_status",
          summary: {
            text: "Waiting for permission to run a command",
            truncated: false,
          },
          opened_at_ms: 100,
          subject_alias: "claude",
          message_id: null,
        },
      ],
    }),
  ];
}
const drawer = (ui) => ui.page.locator("aside.peek");
const screenText = (ui) =>
  ui.page.getByRole("region", { name: "Screen text", exact: true });
const focusedIn = (locator) =>
  locator.evaluate((element) => element.contains(document.activeElement));

test("UI-4: All activity lists blocked, then waiting, then working rows, and its URL survives reload and Back", async (t) => {
  const ui = await open(t, (client) => replay(client, busyTeam()));
  await ui.live();
  assert.equal(await ui.channels.getByRole("button").count(), 2);
  const entry = ui.page.getByRole("link", { name: /^All activity/ });
  assert.equal(await entry.textContent(), "All activity3");
  await entry.click();
  await ui.page
    .getByRole("heading", { name: "All activity", level: 1 })
    .waitFor();
  assert.equal(new URL(ui.page.url()).search, "?view=activity");
  assert.equal(await entry.getAttribute("aria-current"), "page");
  assert.equal(await ui.channels.locator('[aria-current="page"]').count(), 0);
  assert.equal(
    await ui.page.evaluate(() => history.length - window.initialHistoryLength),
    1,
  );
  const rows = ui.page.locator(".activity-row");
  assert.deepEqual(
    await rows.evaluateAll((all) =>
      all.map(
        (row) => row.querySelector(".activity-alias")?.firstChild?.textContent,
      ),
    ),
    ["@claude", "@lead", "@codex"],
  );
  assert.deepEqual(
    await ui.page.locator(".activity-group .nav-label").allTextContents(),
    ["Needs you1", "Waiting1", "Working1"],
  );
  assert.equal(
    await ui.page.locator(".channel-header .health").textContent(),
    "1 needs you · 1 waiting · 1 working",
  );
  const lead = rows.nth(1);
  assert.equal(
    await lead.locator(".activity-text").textContent(),
    "Waiting on @codex",
  );
  assert.match(
    await lead.locator(".activity-since").textContent(),
    /^since \d/,
  );
  assert.equal(
    await rows.nth(0).locator(".activity-text").textContent(),
    "Waiting for permission to run a command",
  );
  assert.equal(await rows.nth(0).locator(".activity-since").count(), 0);
  await ui.page.reload();
  await ui.page
    .getByRole("heading", { name: "All activity", level: 1 })
    .waitFor();
  await ui.live();
  await ui.page.waitForTimeout(2200);
  assert.equal(
    new URL(ui.page.url()).search,
    "?view=activity",
    "landing never moves the reader",
  );
  assert.equal(await rows.count(), 3);
  await ui.row(2).click();
  await ui.page.locator(`[data-selected-channel="${key(2)}"]`).waitFor();
  await ui.page.goBack();
  await ui.page
    .getByRole("heading", { name: "All activity", level: 1 })
    .waitFor();
  await ui.page.goBack();
  await ui.page.waitForFunction(() => location.search === "");
  await ui.page.locator("[data-selected-channel]").waitFor();
});

test("UI-4: a row's channel button opens that channel at its newest message, and every agent idle reads as such", async (t) => {
  const team = busyTeam();
  team[0].frame.messages = Array.from({ length: 40 }, (_, n) =>
    message(`row${n}`, `Row ${n}\n\nmore`),
  );
  const ui = await open(t, (client) => replay(client, team), undefined, {
    path: "/?view=activity",
  });
  await ui.live();
  await ui.page
    .locator(".activity-channel", { hasText: "#alpha" })
    .first()
    .click();
  await ui.page.locator(`[data-selected-channel="${key(1)}"]`).waitFor();
  assert.equal(new URL(ui.page.url()).pathname, `/w/${key(1)}`);
  await ui.page.locator('[data-message-id="row39"]').waitFor();
  assert.equal(
    await ui.page
      .locator(".timeline")
      .evaluate((element) => element === document.activeElement),
    true,
    "focus lands in the timeline, not the page body",
  );
  assert.ok(
    await ui.page
      .locator(".timeline")
      .evaluate(
        (element) =>
          element.scrollHeight - element.scrollTop - element.clientHeight <= 24,
      ),
    "the newest message is in view",
  );
  await ui.page.goBack();
  await ui.page
    .getByRole("heading", { name: "All activity", level: 1 })
    .waitFor();
  const idle = busyTeam().map((value) => {
    for (const lane of value.frame.participants) lane.activity = "idle";
    value.frame.attention = [];
    return value;
  });
  for (const value of idle) ui.connections[0].send("space", value);
  await ui.page
    .getByRole("heading", { name: "Every agent is idle", level: 2 })
    .waitFor();
  assert.equal(
    await ui.page.getByRole("link", { name: /^All activity/ }).textContent(),
    "All activity",
  );
});

test("UI-4: Enter on a member chip opens the drawer with focus on the screen, and Escape closes it with focus back on the chip", async (t) => {
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: () => ({
      body: "› Reading README.md\n• Ran npm test\n  └ 144 passed",
    }),
  });
  await ui.live();
  const chip = ui.page
    .locator(".members")
    .getByRole("button", { name: /^C?@codex/ });
  await chip.focus();
  await ui.page.keyboard.press("Enter");
  await screenText(ui).getByText("144 passed").waitFor();
  assert.equal(await drawer(ui).getAttribute("aria-label"), "Screen of @codex");
  assert.equal(await ui.page.locator("aside.thread").count(), 0);
  assert.equal(
    await screenText(ui).evaluate(
      (element) => element === document.activeElement,
    ),
    true,
  );
  assert.deepEqual(
    ui.screens.requests.map(({ key: at, address }) => [at, address]),
    [[key(1), "codex"]],
  );
  assert.match(
    await drawer(ui).locator(".peek-where").textContent(),
    /^in #alpha · Read at \d/,
  );
  // The mark lands two frames after the text paints; Escape before then cancels it.
  await ui.page.waitForFunction(
    () =>
      performance.getEntriesByName("namedrop:click-to-peek-text", "measure")
        .length > 0,
  );
  await ui.page.keyboard.press("Escape");
  await drawer(ui).waitFor({ state: "detached" });
  assert.equal(
    await chip.evaluate((element) => element === document.activeElement),
    true,
  );
  const you = ui.page.locator('.members li[data-address-id="human"]');
  assert.equal(
    await you.getByRole("button").count(),
    0,
    "the operator chip is not a button",
  );
  const times = await measures(ui.page, "namedrop:click-to-peek-text");
  assert.equal(times.length, 1);
  assert.ok(times[0] > 0 && times[0] < 5000, `click to peek text ${times[0]}`);
  assert.ok((await measures(ui.page, "namedrop:activity-derive")).length > 0);
});

test("UI-4: Refresh makes exactly one request, keeps the earlier text while it reads, and no timer ever reads", async (t) => {
  let release = () => {};
  let reads = 0;
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: async () => {
      reads += 1;
      if (reads === 2) await new Promise((resolve) => (release = resolve));
      return { body: `read ${reads}` };
    },
  });
  await ui.live();
  await ui.page.clock.install();
  await ui.page
    .locator(".members")
    .getByRole("button", { name: /@codex/ })
    .click();
  await screenText(ui).getByText("read 1").waitFor();
  await ui.page.clock.runFor(60_000);
  await ui.page.waitForTimeout(300);
  assert.equal(ui.screens.requests.length, 1, "a minute open makes no request");
  const refresh = drawer(ui).getByRole("button", { name: "Refresh" });
  await refresh.click();
  await until(() => ui.screens.requests.length === 2, "the refresh request");
  assert.equal(await refresh.getAttribute("aria-disabled"), "true");
  await refresh.click({ force: true });
  await refresh.click({ force: true });
  await ui.page.clock.runFor(500);
  assert.equal(await screenText(ui).textContent(), "read 1");
  assert.match(
    await drawer(ui).locator(".peek-where").textContent(),
    /Reading…$/,
  );
  await ui.page.waitForTimeout(300);
  assert.equal(
    ui.screens.requests.length,
    2,
    "clicks during a read make no request",
  );
  release();
  await screenText(ui).getByText("read 2").waitFor();
  assert.equal(await refresh.getAttribute("aria-disabled"), null);
  assert.equal(ui.screens.maxInFlight(), 1);
  await ui.page.clock.runFor(60_000);
  await ui.page.waitForTimeout(300);
  assert.equal(ui.screens.requests.length, 2);
});

test("UI-4: a slow first read shows nothing for 400 ms, then names the screen, then the busy line after 2 s", async (t) => {
  let release = () => {};
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: async ({ address }) => {
      if (address === "lead") return { body: "fast", delayMs: 20 };
      await new Promise((resolve) => (release = resolve));
      return { body: "slow at last" };
    },
  });
  await ui.live();
  await ui.page.evaluate(() => {
    window.sawReading = false;
    new MutationObserver(() => {
      if (document.querySelector(".peek-note")) window.sawReading = true;
    }).observe(document.body, {
      subtree: true,
      childList: true,
      characterData: true,
    });
  });
  const members = ui.page.locator(".members");
  await members.getByRole("button", { name: /@lead/ }).click();
  await screenText(ui).getByText("fast").waitFor();
  assert.equal(
    await ui.page.evaluate(() => window.sawReading),
    false,
    "a fast read never flashes",
  );
  const started = Date.now();
  await members.getByRole("button", { name: /@codex/ }).click();
  const note = drawer(ui).locator(".peek-note");
  await note.getByText("Reading @codex's screen…", { exact: true }).waitFor();
  assert.ok(
    Date.now() - started >= 380,
    `reading shown after ${Date.now() - started} ms`,
  );
  assert.equal(
    await note.getByText("Busy panes can take a few seconds.").count(),
    0,
  );
  await note
    .getByText("Busy panes can take a few seconds.", { exact: true })
    .waitFor();
  assert.ok(
    Date.now() - started >= 1900,
    `busy shown after ${Date.now() - started} ms`,
  );
  assert.equal(
    await screenText(ui).textContent(),
    "",
    "the earlier target's text never leaks",
  );
  release();
  await screenText(ui).getByText("slow at last").waitFor();
  assert.equal(await note.count(), 0);
});

test("a recent retired member opens Pane closed, then leaves the strip after grace while its history author stays", async (t) => {
  const value = team(1, "alpha");
  const member = value.frame.participants.find(
    ({ address_id }) => address_id === "codex",
  );
  assert.ok(member);
  member.activity = "retired";
  member.recently_closed = true;
  value.frame.messages = [
    message("old-author", "Saved before closing", { author: "codex" }),
  ];
  const ui = await open(t, (client) => replay(client, [value]), undefined, {
    onScreen: () => ({
      status: 410,
      body: "This participant's pane is no longer available.\n",
    }),
  });
  await ui.live();
  const chip = ui.page.locator('.members li[data-address-id="codex"]');
  await chip.getByText("Retired", { exact: true }).waitFor();
  await chip.getByRole("button").click();
  await drawer(ui).getByText("Pane closed", { exact: true }).waitFor();
  assert.deepEqual(
    ui.screens.requests.map(({ address }) => address),
    ["codex"],
  );
  const expired = structuredClone(value);
  expired.frame.participants.find(
    ({ address_id }) => address_id === "codex",
  ).recently_closed = false;
  ui.connections[0].send("space", expired);
  await chip.waitFor({ state: "detached" });
  const history = ui.page.locator('[data-message-id="old-author"]');
  await history.getByText("@codex", { exact: true }).waitFor();
  await history.getByText("Saved before closing", { exact: true }).waitFor();
});

test("UI-4: a closed pane reads Pane closed, a busy Herdr offers Try again, and session failure replaces text", async (t) => {
  const answers = [
    { status: 410, body: "This participant's pane is no longer available.\n" },
    { status: 503, body: "Screen is temporarily unavailable.\n" },
    { body: "earlier text" },
    { status: 503, body: "Screen is temporarily unavailable.\n" },
    { status: 401, body: "This dashboard needs its key.\n" },
  ];
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: () => answers.shift() ?? { body: "" },
  });
  await ui.live();
  const chip = ui.page
    .locator(".members")
    .getByRole("button", { name: /@codex/ });
  await chip.click();
  const failure = drawer(ui).locator(".peek-failure");
  await failure.getByText("Pane closed", { exact: true }).waitFor();
  await failure
    .getByText("@codex no longer has a pane Namedrop can read.", {
      exact: true,
    })
    .waitFor();
  assert.equal(await failure.getByRole("button").count(), 0);
  await drawer(ui).getByRole("button", { name: "Refresh" }).click();
  await failure
    .getByText("Couldn't read the screen", { exact: true })
    .waitFor();
  await failure
    .getByText("Herdr was busy or didn't answer in time.", { exact: true })
    .waitFor();
  await failure.getByRole("button", { name: "Try again", exact: true }).click();
  await screenText(ui).getByText("earlier text").waitFor();
  await drawer(ui).getByRole("button", { name: "Refresh" }).click();
  const bar = drawer(ui).locator(".peek-bar");
  await bar.getByText("Couldn't read the screen", { exact: true }).waitFor();
  assert.equal(await screenText(ui).textContent(), "earlier text");
  assert.match(
    await drawer(ui).locator(".peek-where").textContent(),
    /Read at \d/,
  );
  await bar.getByRole("button", { name: "Try again", exact: true }).click();
  await failure.getByText("Session ended", { exact: true }).waitFor();
  assert.equal(await failure.getByRole("button").count(), 0);
  assert.equal(await screenText(ui).textContent(), "");
  assert.equal(await bar.count(), 0);
  assert.equal(ui.screens.requests.length, 5);
});

test("UI-4: terminal text that looks like markup renders as text", async (t) => {
  const hostile =
    '<img src=x onerror="window.pwned = 1"><script>window.pwned = 2</script>\n<dialog open>hi</dialog> **bold** https://example.com';
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: () => ({ body: `[Earlier screen text omitted]\n${hostile}` }),
  });
  await ui.live();
  await ui.page
    .locator(".members")
    .getByRole("button", { name: /@codex/ })
    .click();
  await screenText(ui)
    .locator("pre")
    .getByText("**bold**", { exact: false })
    .waitFor();
  assert.equal(await screenText(ui).locator("pre").textContent(), hostile);
  assert.equal(
    await drawer(ui).locator("img, script, dialog, a, strong").count(),
    0,
  );
  assert.equal(await ui.page.locator("dialog").count(), 0);
  assert.equal(await ui.page.evaluate(() => window.pwned), undefined);
  await drawer(ui)
    .getByText("Earlier lines not shown", { exact: true })
    .waitFor();
});

test("UI-4: names in a message header open the drawer, which replaces the thread pane and the other way round", async (t) => {
  const team = busyTeam();
  team[0].frame.messages = [
    message("ask", "check the diff", {
      legs: [{ to: "codex", stage: { stage: "queued" } }],
    }),
    message("answer", "looks fine", {
      author: "codex",
      thread_id: "ask",
      in_reply_to: "ask",
      kind: "reply",
    }),
  ];
  team[0].frame.threads = [
    {
      thread_id: "ask",
      title: { text: "check", truncated: false },
      message_count: 2,
    },
  ];
  const ui = await open(t, (client) => replay(client, team), undefined, {
    onScreen: () => ({ body: "codex screen" }),
  });
  await ui.live();
  const timeline = ui.page.locator(".timeline");
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .author').textContent(),
    "You",
  );
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .author button').count(),
    0,
  );
  await timeline.getByRole("button", { name: /^1 reply from @codex/ }).click();
  const thread = ui.page.locator("aside.thread");
  await thread.waitFor();
  await thread
    .locator('[data-message-id="answer"] .author')
    .getByRole("button", { name: "@codex" })
    .click();
  await screenText(ui).getByText("codex screen").waitFor();
  assert.equal(await thread.count(), 0);
  assert.equal(await ui.page.locator("aside.peek.thread").count(), 0);
  await timeline
    .locator('[data-message-id="ask"] .to')
    .getByRole("button", { name: "@codex" })
    .waitFor();
  await ui.page.keyboard.press("Escape");
  await drawer(ui).waitFor({ state: "detached" });
  assert.equal(
    await ui.page.evaluate(
      (key) =>
        document.activeElement?.closest("main") !== null &&
        document.activeElement?.getAttribute("data-peek") === key,
      `${key(1)}/codex`,
    ),
    true,
    "focus moves to an equivalent control when the opener is gone",
  );
  await timeline
    .locator('[data-message-id="ask"] .to')
    .getByRole("button", { name: "@codex" })
    .click();
  await drawer(ui).waitFor();
  await timeline.getByRole("button", { name: /^1 reply from @codex/ }).click();
  await thread.waitFor();
  assert.equal(await drawer(ui).count(), 0);
});

test("UI-4: the drawer is 480 wide at 1440 and 400 at 1024, opening a row selects it, and nothing scrolls sideways", async (t) => {
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    path: "/?view=activity",
    onScreen: () => ({ body: `${"x".repeat(900)}\n${"word ".repeat(300)}` }),
  });
  await ui.live();
  const row = ui.page.locator('.activity-row[data-peek$="/codex"]');
  await row.click();
  await screenText(ui).locator("pre").waitFor();
  assert.equal(await row.getAttribute("aria-expanded"), "true");
  assert.equal(await focusedIn(drawer(ui)), true);
  for (const [width, height, expected] of [
    [1440, 900, 480],
    [1024, 700, 400],
  ]) {
    await ui.page.setViewportSize({ width, height });
    const layout = await ui.page.evaluate(() => ({
      drawer: Math.round(
        document.querySelector("aside.peek").getBoundingClientRect().width,
      ),
      page:
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
      text: (() => {
        const element = document.querySelector(".peek-text");
        return element.scrollWidth - element.clientWidth;
      })(),
    }));
    assert.deepEqual(layout, { drawer: expected, page: 0, text: 0 });
  }
  await ui.page.keyboard.press("Escape");
  await drawer(ui).waitFor({ state: "detached" });
  assert.equal(
    await row.evaluate((element) => element === document.activeElement),
    true,
  );
  assert.equal(await row.getAttribute("aria-expanded"), "false");
});

test("CL-K: reopening shows the last screen and original stamp before a fresh full response", async (t) => {
  let release = () => {};
  let reads = 0;
  const ui = await open(t, (client) => replay(client, busyTeam()), undefined, {
    onScreen: async () => {
      const read = ++reads;
      if (read === 2) await new Promise((resolve) => (release = resolve));
      return { body: `full screen ${read}` };
    },
  });
  t.after(() => release());
  await ui.live();
  await ui.page.clock.install({ time: new Date("2026-10-29T12:00:00Z") });
  const chip = ui.page
    .locator(".members")
    .getByRole("button", { name: /@codex/ });
  await chip.click();
  await screenText(ui).getByText("full screen 1").waitFor();
  const stamp = await drawer(ui).locator(".peek-where").textContent();
  await ui.page.keyboard.press("Escape");
  await drawer(ui).waitFor({ state: "detached" });
  await ui.page.clock.setFixedTime(new Date("2026-10-29T12:02:00Z"));
  await chip.click();
  await until(
    () => ui.screens.requests.length === 2,
    "the fresh reopen request",
  );
  assert.equal(
    await screenText(ui).textContent(),
    "full screen 1",
    "reopen retains successful text while the full read is pending",
  );
  assert.equal(
    await drawer(ui).locator(".peek-where").textContent(),
    stamp,
    "reopen keeps the original read time",
  );
  assert.equal(
    await drawer(ui)
      .getByRole("button", { name: "Refresh" })
      .getAttribute("aria-disabled"),
    "true",
  );
  release();
  await screenText(ui).getByText("full screen 2").waitFor();
  assert.notEqual(await drawer(ui).locator(".peek-where").textContent(), stamp);
  assert.deepEqual(
    ui.screens.requests.map(({ key: channel, address }) => [channel, address]),
    [
      [key(1), "codex"],
      [key(1), "codex"],
    ],
  );
});

let peekHarnessBuild;
async function openPeekHarness(t) {
  peekHarnessBuild ??= build({
    root: fileURLToPath(new URL("..", import.meta.url)),
    configFile: false,
    logLevel: "silent",
    build: { write: false, rollupOptions: { input: "virtual:peek-harness" } },
    plugins: [
      {
        name: "peek-harness",
        resolveId(id) {
          if (id === "virtual:peek-harness") return "\0peek-harness";
        },
        load(id) {
          if (id !== "\0peek-harness") return;
          return `
          import { createElement } from "react";
          import { createRoot } from "react-dom/client";
          import { PeekDrawer } from "./src/ui/PeekDrawer.tsx";
          const root = createRoot(document.getElementById("root"));
          const requests = [];
          const readers = Array.from({length: 2}, () => (target, signal) =>
            new Promise(resolve => requests.push({ target, signal, resolve })));
          const close = () => root.render(null);
          window.peekHarness = {
            requests,
            close,
            open(member = "codex", channel = "alpha", reader = 0) {
              root.render(createElement(PeekDrawer, {
                target: { channel, member, alias: "@" + member, kind: "agent" },
                who: { alias: "@" + member, kind: "agent", activity: "working", channel: {label: "#" + channel, suffix: null} },
                reader: readers[reader], startedAt: performance.now(), paneId: "peek", onClose: close,
              }));
            },
            settle(index, text, failure = null) {
              requests[index].resolve(failure === null
                ? {kind: "text", screen: {text, earlierOmitted: false}}
                : {kind: "failed", failure});
            },
          };
        `;
        },
      },
    ],
  });
  const bundle = await peekHarnessBuild;
  const source = bundle.output.find((entry) => entry.type === "chunk").code;
  const server = createServer((req, res) => {
    res.writeHead(200, {
      "Content-Type": req.url === "/peek.js" ? "text/javascript" : "text/html",
    });
    res.end(
      req.url === "/peek.js"
        ? source
        : '<div id="root"></div><script type="module" src="/peek.js"></script>',
    );
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const page = await browser.newPage();
  page.setDefaultTimeout(5000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  t.after(async () => {
    await page.close();
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    assert.deepEqual(errors, []);
  });
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.waitForFunction(() => window.peekHarness !== undefined);
  let reads = 0;
  return {
    page,
    async show(member = "codex", channel = "alpha", reader = 0) {
      await page.evaluate(
        ([member, channel, reader]) =>
          window.peekHarness.open(member, channel, reader),
        [member, channel, reader],
      );
      await page.waitForFunction(
        (count) => window.peekHarness.requests.length === count,
        ++reads,
      );
    },
    async close() {
      await page.evaluate(() => window.peekHarness.close());
      await page.locator("aside.peek").waitFor({ state: "detached" });
    },
    async settle(text, failure = null, index = reads - 1) {
      await page.evaluate(
        ([index, text, failure]) =>
          window.peekHarness.settle(index, text, failure),
        [index, text, failure],
      );
      if (failure === null) {
        await page.waitForFunction(
          () =>
            document
              .querySelector(".peek-refresh")
              ?.getAttribute("aria-disabled") === null,
        );
      } else {
        await page.locator('[role="status"]').waitFor();
      }
    },
    text: () =>
      page
        .getByRole("region", { name: "Screen text", exact: true })
        .textContent(),
  };
}

test("CL-K: targets, channels and readers isolate retained text, including reader replacement while mounted", async (t) => {
  const ui = await openPeekHarness(t);
  await ui.show();
  await ui.settle("alpha codex");
  await ui.show("lead");
  assert.equal(await ui.text(), "");
  await ui.settle("alpha lead");
  await ui.show("codex", "beta");
  assert.equal(await ui.text(), "");
  await ui.settle("beta codex");
  await ui.show();
  assert.equal(await ui.text(), "alpha codex");
  await ui.settle("alpha codex fresh");
  await ui.show("codex", "alpha", 1);
  assert.equal(
    await ui.text(),
    "",
    "replacing the reader resets mounted state",
  );
  await ui.settle("second reader");
  await ui.show();
  assert.equal(await ui.text(), "alpha codex fresh");
  await ui.show("codex", "beta");
  assert.equal(await ui.text(), "beta codex");
  await ui.settle("", "session");
  await ui.show("codex", "alpha", 1);
  assert.equal(
    await ui.text(),
    "second reader",
    "session failure clears only its reader scope",
  );
});

test("CL-K: a late aborted completion cannot replace mounted text or retained snapshots", async (t) => {
  const ui = await openPeekHarness(t);
  await ui.show();
  await ui.settle("accepted");
  await ui.close();
  await ui.show();
  await ui.close();
  await ui.show("lead");
  await ui.settle("lead accepted");
  assert.equal(
    await ui.page.evaluate(() => window.peekHarness.requests[1].signal.aborted),
    true,
  );
  await ui.page.evaluate(() => window.peekHarness.settle(1, "late aborted"));
  assert.equal(await ui.text(), "lead accepted");
  await ui.show();
  assert.equal(
    await ui.text(),
    "accepted",
    "the aborted completion never enters retention",
  );
  await ui.close();
  await ui.page.evaluate(() => window.peekHarness.settle(3, "", "session"));
  await ui.show("lead");
  assert.equal(
    await ui.text(),
    "lead accepted",
    "an aborted session failure cannot clear retention",
  );
});

test("CL-K: successful empty reads replace prior retained text", async (t) => {
  const ui = await openPeekHarness(t);
  await ui.show();
  await ui.settle("prior");
  await ui.close();
  await ui.show();
  await ui.settle("");
  await ui.page.getByText("The screen is empty.", { exact: true }).waitFor();
  await ui.close();
  await ui.show();
  assert.equal(await ui.text(), "");
  await ui.page.getByText("The screen is empty.", { exact: true }).waitFor();
});

test("CL-K: transient failures preserve retention, target failures remove one screen, and session failure clears the reader", async (t) => {
  const ui = await openPeekHarness(t);
  await ui.show("lead");
  await ui.settle("lead retained");
  for (const failure of [
    "busy",
    "unreachable",
    "missing",
    "closed",
    "session",
  ]) {
    await ui.show();
    await ui.settle("codex retained");
    await ui.close();
    await ui.show();
    await ui.settle("", failure);
    assert.equal(
      await ui.text(),
      failure === "busy" || failure === "unreachable" ? "codex retained" : "",
      "only transient failures keep mounted text",
    );
    await ui.close();
    await ui.show();
    assert.equal(
      await ui.text(),
      failure === "busy" || failure === "unreachable" ? "codex retained" : "",
    );
    await ui.show("lead");
    assert.equal(await ui.text(), failure === "session" ? "" : "lead retained");
    await ui.close();
  }
});

test("CL-K: retention holds at most eight screens and reads do not renew insertion order", async (t) => {
  const ui = await openPeekHarness(t);
  for (let n = 0; n < 8; n++) {
    await ui.show(`member${n}`);
    await ui.settle(`screen ${n}`);
  }
  await ui.show("member0");
  assert.equal(await ui.text(), "screen 0");
  await ui.show("member8");
  await ui.settle("screen 8");
  await ui.show("member0");
  assert.equal(
    await ui.text(),
    "",
    "a cache read does not prevent oldest-entry eviction",
  );
  await ui.show("member1");
  assert.equal(await ui.text(), "screen 1");
  await ui.settle("member1 updated");
  await ui.show("member9");
  await ui.settle("screen 9");
  await ui.show("member2");
  assert.equal(await ui.text(), "");
  await ui.show("member1");
  assert.equal(
    await ui.text(),
    "member1 updated",
    "successful writes renew insertion order",
  );
});

test("CL-K: character bounds evict old text and oversized success invalidates a prior snapshot", async (t) => {
  const ui = await openPeekHarness(t);
  await ui.show("oldest");
  await ui.settle("old");
  await ui.show("largest");
  await ui.settle("x".repeat(512 * 1024));
  await ui.show("oldest");
  assert.equal(
    await ui.text(),
    "",
    "total retained characters stay within 512 Ki",
  );
  await ui.show("largest");
  assert.equal(
    (await ui.text()).length,
    512 * 1024,
    "the exact character limit is retained",
  );
  await ui.settle("y".repeat(512 * 1024 + 1));
  assert.equal(
    (await ui.text()).length,
    512 * 1024 + 1,
    "oversized success still renders",
  );
  await ui.close();
  await ui.show("largest");
  assert.equal(
    await ui.text(),
    "",
    "oversized success removes its previous snapshot",
  );
});

for (const status of [404, 410, 401, 403]) {
  test(`CL-K: HTTP ${status} invalidates retained text at the parsed failure boundary`, async (t) => {
    let release = () => {};
    const gate = new Promise((resolve) => (release = resolve));
    const answers = {
      codex: [{ body: "codex retained" }, { status }, null],
      lead: [{ body: "lead retained" }, null],
    };
    const ui = await open(
      t,
      (client) => replay(client, busyTeam()),
      undefined,
      {
        onScreen: async ({ address }) => {
          const answer = answers[address].shift();
          if (answer !== null) return answer;
          await gate;
          return { body: "fresh" };
        },
      },
    );
    t.after(() => release());
    await ui.live();
    const members = ui.page.locator(".members");
    const codex = members.getByRole("button", { name: /@codex/ });
    const lead = members.getByRole("button", { name: /@lead/ });
    await codex.click();
    await screenText(ui).getByText("codex retained").waitFor();
    await lead.click();
    await screenText(ui).getByText("lead retained").waitFor();
    await codex.click();
    await drawer(ui).locator(".peek-failure").waitFor();
    assert.equal(await screenText(ui).textContent(), "");
    await ui.page.keyboard.press("Escape");
    await drawer(ui).waitFor({ state: "detached" });
    await codex.click();
    await until(
      () => ui.screens.requests.length === 4,
      "the uncached target read",
    );
    assert.equal(await screenText(ui).textContent(), "");
    await lead.click();
    await until(
      () => ui.screens.requests.length === 5,
      "the other target read",
    );
    assert.equal(
      await screenText(ui).textContent(),
      status === 401 || status === 403 ? "" : "lead retained",
    );
  });
}

for (const status of [404, 410, 401, 403]) {
  test(`CL-K fix 1: HTTP ${status} replaces retained text and its old stamp after reopen`, async (t) => {
    let release = () => {};
    let reads = 0;
    const gate = new Promise((resolve) => (release = resolve));
    const ui = await open(
      t,
      (client) => replay(client, busyTeam()),
      undefined,
      {
        onScreen: async () => {
          if (++reads === 1) return { body: "old screen before pane closes" };
          await gate;
          return { status };
        },
      },
    );
    t.after(() => release());
    await ui.live();
    const chip = ui.page
      .locator(".members")
      .getByRole("button", { name: /@codex/ });
    await chip.click();
    await screenText(ui).getByText("old screen before pane closes").waitFor();
    await ui.page.keyboard.press("Escape");
    await drawer(ui).waitFor({ state: "detached" });
    await chip.click();
    await until(
      () => ui.screens.requests.length === 2,
      "the reopen screen request",
    );
    assert.equal(
      await screenText(ui).textContent(),
      "old screen before pane closes",
    );
    release();
    await ui.page.waitForFunction(
      () =>
        document
          .querySelector(".peek-refresh")
          ?.getAttribute("aria-disabled") === null,
    );
    assert.equal(
      await screenText(ui).textContent(),
      "",
      "failed target or session must remove retained screen text",
    );
    assert.equal(await drawer(ui).locator(".peek-bar").count(), 0);
    assert.equal(await drawer(ui).locator(".peek-failure").count(), 1);
    assert.doesNotMatch(
      await drawer(ui).locator(".peek-where").textContent(),
      /Read at/,
    );
    assert.equal(ui.screens.requests.length, 2);
  });
}

function actionableAttention(kind = "reply") {
  const value = frame(1, "alpha", "needs_you");
  value.frame.participants = people();
  const root = message("attention-root", "Original request", {
    at_ms: 1,
    legs: [{ to: "codex", stage: { stage: "delivered" } }],
  });
  const target = message(
    "attention-target",
    "The decision needing a response",
    {
      thread_id: root.message_id,
      in_reply_to: root.message_id,
      at_ms: 2,
      author: "codex",
      kind: "reply",
    },
  );
  const newer = message("attention-newer", "A newer reply in the same thread", {
    thread_id: root.message_id,
    in_reply_to: target.message_id,
    at_ms: 3,
    author: "codex",
    kind: "reply",
  });
  value.frame.messages = [root, target, newer];
  const conversation = {
    thread_id: root.message_id,
    message_id: target.message_id,
  };
  value.frame.attention = [
    {
      source_kind: kind === "reply" ? "delivery_attempt" : "coordination",
      summary: { text: "A response is needed.", truncated: false },
      opened_at_ms: 4,
      subject_alias: null,
      message_id: target.message_id,
      conversation,
      coordination_id: kind === "message" ? "decision-1" : null,
      response:
        kind === "reply"
          ? { kind, ...conversation, address_id: "codex" }
          : { kind, address_id: "codex" },
    },
  ];
  return {
    value,
    context: {
      thread_id: root.message_id,
      message_count: 3,
      earlier_omitted: false,
      messages: [root, target, newer],
    },
  };
}

test("CL-A: legacy loaded navigation never requests context and restores focus", async (t) => {
  const ui = await open(t, (client) => replay(client, [locatedChannel()]));
  await ui.live();
  const show = ui.page.getByRole("button", {
    name: "Show message from @codex",
    exact: true,
  });
  await show.click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  await pane.locator('[data-message-id="m20"]').waitFor();
  assert.deepEqual(ui.contextRequests, []);
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
});

test("CL-A: explicit attention names its responsible member and focuses fetched evidence", async (t) => {
  const { value, context } = actionableAttention();
  value.frame.participants[0].alias = "@current-owner";
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    { onContext: () => context },
  );
  await ui.live();
  assert.equal(
    await ui.page
      .getByRole("button", { name: "Respond to @current-owner", exact: true })
      .count(),
    1,
  );
  await ui.page
    .getByRole("button", {
      name: "Show message from @current-owner",
      exact: true,
    })
    .click();
  const target = ui.page
    .getByRole("complementary", { name: "Thread", exact: true })
    .locator('[data-message-id="attention-target"]');
  await target.waitFor();
  assert.equal(
    await target.evaluate((row) => row === document.activeElement),
    true,
  );
  assert.equal(
    ui.contextRequests.length,
    1,
    "explicit metadata fetches even when target is loaded",
  );
  assert.equal(
    ui.contextRequests[0].pathname,
    `/w/${key(1)}/messages/attention-target/context`,
  );
});

test("CL-A: Respond posts the exact attention target despite newer context and SSE replies", async (t) => {
  const { value, context } = actionableAttention();
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    { onContext: () => context },
  );
  await ui.live();
  await ui.page.getByRole("button", { name: /^Respond/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  await pane.locator('[data-message-id="attention-newer"]').waitFor();
  const reply = pane.getByRole("combobox", { name: "Reply to @codex" });
  await reply.fill("@codex Keep the selected decision");
  const next = structuredClone(value);
  next.frame.messages.push(
    message("attention-live", "An even newer live reply", {
      thread_id: "attention-root",
      in_reply_to: "attention-newer",
      at_ms: Date.now(),
      author: "codex",
      kind: "reply",
    }),
  );
  ui.connections[0].send("space", next);
  await pane.locator('[data-message-id="attention-live"]').waitFor();
  await reply.press("Enter");
  await until(
    () => ui.posts.attempts.some((attempt) => attempt.body !== null),
    "the attention reply POST",
  );
  assert.equal(ui.posts.attempts[0].body.in_reply_to, "attention-target");
  assert.equal(
    ui.posts.attempts[0].body.source,
    "@codex Keep the selected decision",
  );
});

function replyLimit({ thread_id = "t1", actions = ["continue", "stop"] } = {}) {
  const value = team(1, "alpha");
  value.frame.health = { level: "needs_you", items: 1 };
  value.frame.participants[0].activity = "blocked";
  const ask = message("ask", "@codex keep going with @claude", {
    at_ms: 1,
    legs: [{ to: "codex", stage: { stage: "delivered" } }],
  });
  value.frame.messages = [ask];
  value.frame.attention = [
    {
      attention_id: "limit-1",
      source_kind: "automatic_reply_limit",
      summary: { text: "Paused after 10 agent replies.", truncated: false },
      opened_at_ms: Date.now() - 60_000,
      subject_alias: "codex",
      message_id: "ask",
      ...(thread_id === null ? {} : { thread_id }),
      ...(actions === null ? {} : { actions }),
    },
  ];
  return value;
}
const { continue: allowCopy, stop: stopCopy } = threadControlCopy;
function limitItem(ui) {
  const item = ui.page
    .getByRole("region", { name: "Attention needed" })
    .locator(".attention-item");
  return {
    item,
    allow: item.getByRole("button", { name: allowCopy.label }),
    stop: item.getByRole("button", { name: stopCopy.label, exact: true }),
    confirm: item.getByRole("button", {
      name: threadControlCopy.confirmStop,
      exact: true,
    }),
    cancel: item.getByRole("button", { name: threadControlCopy.cancel }),
    alert: item.getByRole("alert"),
    labels: () =>
      item
        .locator(".attention-actions")
        .first()
        .evaluate((row) =>
          [...row.children].map((child) => child.textContent.trim()),
        ),
  };
}
const focusedText = (ui) =>
  ui.page.evaluate(() => document.activeElement?.textContent ?? null);

test("CH-fix-2 item 3: Allow 10 more replies posts continue with the CSRF header, waits on the frame, and the item goes", async (t) => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const value = replyLimit();
  const ui = await open(t, (client) => replay(client, [value]), undefined, {
    onControl: async () => {
      await gate;
      return { status: 200 };
    },
  });
  await ui.live();
  const limit = limitItem(ui);
  await limit.allow.waitFor();
  assert.equal(
    await limit.item.locator(".attention-hint").textContent(),
    "The agents paused after 10 replies in a row.",
  );
  assert.deepEqual(await limit.labels(), [
    "Show message",
    allowCopy.label,
    stopCopy.label,
  ]);
  assert.equal(await limit.allow.getAttribute("title"), allowCopy.title);
  assert.equal(await limit.stop.getAttribute("title"), stopCopy.title);
  assert.equal(
    await limit.allow.evaluate((button) => getComputedStyle(button).fontSize),
    "13px",
  );
  await limit.allow.click();
  await until(() => ui.posts.controls.length === 1, "the continue POST");
  assert.deepEqual(
    (({ method, path, csrf, type, body }) => ({
      method,
      path,
      csrf,
      type,
      body,
    }))(ui.posts.controls[0]),
    {
      method: "POST",
      path: `/w/${key(1)}/threads/t1/continue`,
      csrf: "csrf-1",
      type: "application/json",
      body: "{}",
    },
  );
  const pressed = limit.item.getByRole("button", { name: allowCopy.pending });
  await pressed.waitFor();
  for (const button of await limit.item.getByRole("button").all())
    if ((await button.textContent()) !== "Details")
      assert.equal(await button.isDisabled(), true, await button.textContent());
  release();
  await until(() => ui.posts.controls[0].status === 200, "the answer");
  assert.equal(
    await pressed.isDisabled(),
    true,
    "still pending until the frame drops the item",
  );
  assert.equal(ui.posts.controls.length, 1);
  value.frame.attention = [];
  value.frame.health = { level: "calm" };
  ui.connections[0].send("space", value);
  await limit.item.waitFor({ state: "detached" });
});

for (const action of ["continue", "stop"])
  test(`CH-fix-3 item 2: a ${action} request still open when its item leaves is aborted, and the page posts afterwards`, async (t) => {
    const value = replyLimit();
    const ui = await open(t, (client) => replay(client, [value]), undefined, {
      onControl: () => new Promise(() => {}),
    });
    await ui.live();
    const failed = [];
    ui.page.on("requestfailed", (request) =>
      failed.push([
        new URL(request.url()).pathname,
        request.failure()?.errorText,
      ]),
    );
    const limit = limitItem(ui);
    if (action === "continue") await limit.allow.click();
    else {
      await limit.stop.click();
      await limit.confirm.click();
    }
    await until(() => ui.posts.controls.length === 1, `the ${action} POST`);
    value.frame.attention = [];
    value.frame.health = { level: "calm" };
    ui.connections[0].send("space", value);
    await limit.item.waitFor({ state: "detached" });
    await until(() => failed.length > 0, "the page to abort the request");
    assert.deepEqual(failed, [
      [`/w/${key(1)}/threads/t1/${action}`, "net::ERR_ABORTED"],
    ]);
    await until(
      () => ui.posts.controls[0].closedUnanswered,
      "the server to see the connection close",
    );
    const box = ui.page.locator("main textarea");
    await box.fill("still here");
    await box.press("Enter");
    await until(
      () => ui.posts.attempts[0]?.status === 200,
      "a message POST after the abort",
    );
    assert.equal(JSON.parse(ui.posts.attempts[0].raw).source, "still here");
  });

test("CH-fix-2 item 3: Stop thread asks once inline, Cancel and Escape take it back, and only the confirm posts stop", async (t) => {
  const ui = await open(t, (client) => replay(client, [replyLimit()]));
  await ui.live();
  const limit = limitItem(ui);
  await limit.stop.click();
  await limit.confirm.waitFor();
  assert.deepEqual(await limit.labels(), [
    "Show message",
    threadControlCopy.confirm,
    threadControlCopy.confirmStop,
    threadControlCopy.cancel,
  ]);
  assert.equal(await focusedText(ui), threadControlCopy.confirmStop);
  assert.equal(
    await limit.confirm.evaluate((button) => getComputedStyle(button).color),
    await tokenColor(ui.page, "--failed-text"),
  );
  assert.equal(ui.posts.controls.length, 0, "the first click sends nothing");
  await limit.cancel.click();
  await limit.allow.waitFor();
  assert.equal(await focusedText(ui), stopCopy.label);
  await ui.page.keyboard.press("Enter");
  await limit.confirm.waitFor();
  await ui.page.keyboard.press("Escape");
  await limit.allow.waitFor();
  assert.equal(await focusedText(ui), stopCopy.label, "Escape returns too");
  assert.equal(ui.posts.controls.length, 0);
  await limit.stop.click();
  await limit.confirm.click();
  await until(() => ui.posts.controls.length === 1, "the stop POST");
  assert.equal(ui.posts.controls[0].path, `/w/${key(1)}/threads/t1/stop`);
  assert.equal(ui.posts.controls[0].csrf, "csrf-1");
  await limit.item.getByRole("button", { name: stopCopy.pending }).waitFor();
});

test("CH-fix-2 item 3: a failed press says so under the row and the buttons come back, and a 404 says the thread is gone", async (t) => {
  const outcomes = [500, 200];
  const ui = await open(
    t,
    (client) => replay(client, [replyLimit()]),
    undefined,
    {
      onControl: () => ({ status: outcomes.shift() }),
    },
  );
  await ui.live();
  const limit = limitItem(ui);
  await limit.allow.click();
  await limit.alert.waitFor();
  assert.equal(await limit.alert.textContent(), allowCopy.failed);
  assert.equal(await limit.alert.getAttribute("title"), "HTTP 500");
  assert.equal(
    await limit.alert.evaluate((line) => getComputedStyle(line).fontSize),
    "12px",
  );
  assert.equal(await limit.allow.isDisabled(), false, "the buttons are back");
  await limit.allow.click();
  await limit.item.getByRole("button", { name: allowCopy.pending }).waitFor();
  assert.equal(await limit.alert.count(), 0, "a new press clears the line");
  assert.equal(ui.posts.controls.length, 2);
  const ui2 = await open(
    t,
    (client) => replay(client, [replyLimit()]),
    undefined,
    {
      onControl: () => ({ status: 404 }),
    },
  );
  await ui2.live();
  const gone = limitItem(ui2);
  await gone.stop.click();
  await gone.confirm.click();
  await gone.alert.waitFor();
  assert.equal(await gone.alert.textContent(), threadControlCopy.gone);
  assert.equal(await gone.alert.getAttribute("title"), "HTTP 404");
  assert.equal(await focusedText(ui2), stopCopy.label);
});

test("CH-fix-2 item 3: an item with no thread or no actions keeps the pane hint, and All activity rows show the same controls", async (t) => {
  for (const extra of [
    { thread_id: null },
    { actions: null },
    { actions: [] },
  ]) {
    const ui = await open(t, (client) => replay(client, [replyLimit(extra)]));
    await ui.live();
    const limit = limitItem(ui);
    await limit.item.waitFor();
    assert.equal(
      await limit.item.locator(".attention-hint").textContent(),
      "Continue or stop this thread from its pane.",
      JSON.stringify(extra),
    );
    assert.equal(await limit.allow.count(), 0);
    assert.equal(await limit.stop.count(), 0);
  }
  const ui = await open(
    t,
    (client) => replay(client, [replyLimit({ actions: ["stop"] })]),
    undefined,
    {
      path: "/?view=activity",
    },
  );
  await ui.live();
  const row = ui.page.locator(".activity-item", { hasText: "@codex" });
  await row
    .getByRole("button", { name: stopCopy.label, exact: true })
    .waitFor();
  assert.equal(
    await row.getByRole("button", { name: allowCopy.label }).count(),
    0,
    "only the listed action",
  );
});

test("CH-2: Respond targets its agent with the prefill, and plain text names every agent the reply reaches", async (t) => {
  const { value, context } = actionableAttention();
  value.frame.participants.push({
    ...value.frame.participants[0],
    address_id: "claude",
    alias: "@claude",
    kind: { type: "agent", runtime: "claude" },
  });
  for (const row of [value.frame.messages[0], context.messages[0]])
    row.legs = [
      { to: "codex", stage: { stage: "delivered" } },
      { to: "claude", stage: { stage: "delivered" } },
    ];
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    { onContext: () => context },
  );
  await ui.live();
  await ui.page.getByRole("button", { name: /^Respond/ }).click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const reply = pane.getByRole("combobox");
  await pane.locator('[data-message-id="attention-target"]').waitFor();
  await eventually(
    async () => (await reply.inputValue()) === "@codex ",
    "the Respond prefill",
  );
  await reply.fill("ok go ahead");
  assert.equal(
    await pane.locator(".composer-to").textContent(),
    "To @codex, @claude",
  );
  assert.equal(
    await reply.getAttribute("aria-label"),
    "Reply to @codex, @claude",
  );
});

test("CL-A: preparing an already prepared coordination response keeps the draft without a warning", async (t) => {
  const { value, context } = actionableAttention("message");
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    { onContext: () => context },
  );
  await ui.live();
  await ui.page.getByRole("button", { name: /^Respond/ }).click();
  const draft = ui.page.locator("main .composer").getByRole("combobox");
  await ui.page.waitForFunction(() =>
    document
      .querySelector("main .composer textarea")
      ?.value.includes("Regarding coordination decision-1:"),
  );
  const prepared = await draft.inputValue();
  assert.equal(prepared, "@codex Regarding coordination decision-1: ");
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  await pane
    .getByRole("button", { name: "Prepare response", exact: true })
    .click();
  assert.equal(await draft.inputValue(), prepared);
  assert.equal(
    await ui.page
      .getByText("Your channel draft or pending send was kept.", {
        exact: false,
      })
      .count(),
    0,
  );
  assert.equal(ui.posts.attempts.length, 0);
  await draft.fill("");
  await pane
    .getByRole("button", { name: "Prepare response", exact: true })
    .click();
  await ui.page.waitForFunction(() =>
    document
      .querySelector("main .composer textarea")
      ?.value.includes("Regarding coordination decision-1:"),
  );
  assert.equal(await draft.inputValue(), prepared);
  assert.equal(
    await ui.page
      .getByText("Your channel draft or pending send was kept.", {
        exact: false,
      })
      .count(),
    0,
  );
});

test("CH-2: the page never scrolls with a long channel, at every width", async (t) => {
  const codex = fixture.frame.participants[0].address_id;
  const busy = frame(1, "alpha");
  busy.frame.messages = Array.from({ length: 80 }, (_, index) =>
    message(
      `long-${index}`,
      `@codex message ${index} with enough words to wrap`,
      {
        // Six minutes apart, so no row groups and each draws a header with hidden text.
        at_ms: 1_000 + index * 360_000,
        legs: [{ to: codex, stage: { stage: "delivered" } }],
      },
    ),
  );
  const ui = await open(t, (client) => replay(client, [busy]));
  await ui.page.locator('[data-message-id="long-79"]').waitFor();
  const page = () =>
    ui.page.evaluate(() => {
      window.scrollTo(0, 100_000);
      return {
        overflow:
          document.documentElement.scrollHeight -
          document.documentElement.clientHeight,
        scrolled: window.scrollY,
      };
    });
  for (const [width, height] of [
    [1440, 900],
    [1024, 700],
    [640, 800],
    [390, 844],
    [1152, 720],
  ]) {
    await ui.page.setViewportSize({ width, height });
    assert.deepEqual(
      await page(),
      { overflow: 0, scrolled: 0 },
      `${width}x${height}`,
    );
  }
  await ui.page.setViewportSize({ width: 1440, height: 900 });
  await ui.page.locator('[data-message-id="long-79"]').hover();
  assert.deepEqual(await page(), { overflow: 0, scrolled: 0 });
});

test("CH-2: the page fills the window in every view and width, and only the panes inside it scroll", async (t) => {
  const busy = crew(
    1,
    "alpha",
    Array.from({ length: 40 }, (_, index) => [
      `agent-${index}`,
      `@agent-${index}`,
      "working",
      { mailbox: [inbox("you", `task ${index}`, 1_000 + index)] },
    ]),
  );
  const requests = Array.from({ length: 60 }, (_, index) =>
    message(
      `long-${index}`,
      `@agent-0 message ${index} with enough words to wrap`,
      {
        // Six minutes apart, so no row groups and each draws a header with hidden text.
        at_ms: 1_000 + index * 360_000,
        legs: [{ to: "agent-0", stage: { stage: "delivered" } }],
      },
    ),
  );
  const replies = Array.from({ length: 40 }, (_, index) =>
    message(
      `reply-${index}`,
      `reply ${index} with enough words to wrap a line or two in the thread pane`,
      {
        thread_id: "long-59",
        in_reply_to: index === 0 ? "long-59" : `reply-${index - 1}`,
        kind: "reply",
        author: "agent-0",
        at_ms: 1_000 + 59 * 360_000 + (index + 1) * 1_000,
      },
    ),
  );
  busy.frame.messages = [...requests, ...replies];
  const screen = Array.from({ length: 300 }, (_, index) => `line ${index}`);
  const ui = await open(t, (client) => replay(client, [busy]), undefined, {
    onScreen: () => ({ body: screen.join("\n") }),
  });
  await ui.page.locator('[data-message-id="long-59"]').waitFor();
  const page = () =>
    ui.page.evaluate(() => {
      window.scrollTo(100_000, 100_000);
      const root = document.documentElement;
      return {
        overflow: [
          root.scrollWidth - root.clientWidth,
          root.scrollHeight - root.clientHeight,
        ],
        scrolled: [window.scrollX, window.scrollY],
      };
    });
  const sizes = [
    ["1440x900", 1440, 900],
    ["1024x700", 1024, 700],
    ["640x800", 640, 800],
    ["390x844", 390, 844],
    ["1440x900 at 125% zoom", 1152, 720],
  ];
  const fits = async (view) => {
    for (const [size, width, height] of sizes) {
      await ui.page.setViewportSize({ width, height });
      assert.deepEqual(
        await page(),
        { overflow: [0, 0], scrolled: [0, 0] },
        `${view} at ${size}`,
      );
    }
    await ui.page.setViewportSize({ width: 1440, height: 900 });
  };
  // A mouse wheel over a pane scrolls the pane, at a wide and a phone width.
  const wheels = async (selector, view) => {
    for (const [size, width, height] of [sizes[0], sizes[3]]) {
      await ui.page.setViewportSize({ width, height });
      const pane = ui.page.locator(selector);
      const box = await pane.boundingBox();
      await ui.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      const before = await pane.evaluate((element) => element.scrollTop);
      await ui.page.mouse.wheel(0, before > 0 ? -400 : 400);
      await eventually(
        async () =>
          (await pane.evaluate((element) => element.scrollTop)) !== before,
        `${view} to scroll on the wheel at ${size}`,
      );
      assert.deepEqual(
        await ui.page.evaluate(() => [window.scrollX, window.scrollY]),
        [0, 0],
        `the wheel over ${view} never scrolls the page at ${size}`,
      );
    }
    await ui.page.setViewportSize({ width: 1440, height: 900 });
  };
  // A pane scrolls inside itself: its scrollTop moves and the page's does not.
  const scrolls = async (selector, view) => {
    const moved = await ui.page.locator(selector).evaluate((element) => {
      element.scrollTop = 0;
      const before = element.scrollTop;
      element.scrollTop = element.scrollHeight;
      return { before, after: element.scrollTop, page: window.scrollY };
    });
    assert.ok(moved.after > moved.before, `${view} scrolls inside itself`);
    assert.equal(moved.page, 0, `${view} never scrolls the page`);
  };
  await fits("the channel");
  await scrolls(".timeline", "the timeline");
  await wheels(".timeline", "the timeline");
  await ui.page.locator('[data-message-id="long-59"] .reply-preview').click();
  await ui.page
    .getByRole("complementary", { name: "Thread", exact: true })
    .waitFor();
  await fits("an open thread");
  await scrolls(".thread-body", "the thread");
  await wheels(".thread-body", "the thread");
  await ui.page.keyboard.press("Escape");
  await ui.page
    .locator(".members")
    .getByRole("button", { name: /@agent-0/ })
    .click();
  await screenText(ui).locator("pre").waitFor();
  await fits("the peek drawer");
  await scrolls(".peek-text", "the screen text");
  await ui.page.keyboard.press("Escape");
  await drawer(ui).waitFor({ state: "detached" });
  await ui.page.getByRole("link", { name: /^All activity/ }).click();
  await ui.page
    .getByRole("heading", { name: "All activity", level: 1 })
    .waitFor();
  await fits("All activity");
  await scrolls(".activity-list", "the activity list");
});

const routed = (reason = "build question", router = "claude-haiku") => ({
  state: "routed",
  to: ["claude"],
  reason,
  router,
});
const iconShape = {
  clock: "M8 5v3l2 1.5",
  arrow: "M3 8h10M9 4l4 4-4 4",
  inbox: "M2.5 9.5h3l1 1.5h3l1-1.5h3M3.6 3.5h8.8l1.1 6v3.5h-11V9.5z",
  alert: "M8 5v3.5M8 11h.01",
};
// The server lists the agents a post names, bare, in the order the text names them.
const named = (text) => [
  ...new Set(
    [...text.matchAll(/(?<![\w\\])@([a-z0-9]+(?:-[a-z0-9]+)*)/g)].map(
      ([, alias]) => alias,
    ),
  ),
];
function channelPost(id, text, channel, extra = {}) {
  return message(id, text, {
    channel: { mentioned: named(text), ...channel },
    at_ms: Date.now() - 600_000,
    ...extra,
  });
}
function answer(id, parent, thread, author, text, extra = {}) {
  return message(id, text, {
    thread_id: thread,
    in_reply_to: parent,
    kind: "reply",
    author,
    ...extra,
  });
}
async function chatRoom(t, messages, options) {
  const value = team(1, "alpha");
  value.frame.messages = messages;
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    undefined,
    options,
  );
  await ui.live();
  return { ui, value };
}
function routeOf(ui, id) {
  const route = ui.page.locator(`main [data-message-id="${id}"] .route`);
  return {
    route,
    parts: () => route.locator(".route-parts > li").allTextContents(),
    icon: () =>
      route
        .locator("svg.route-icon path")
        .evaluateAll((paths) => paths.map((path) => path.getAttribute("d"))),
  };
}
async function tokenColor(page, token) {
  return page.evaluate((token) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${token})`;
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, token);
}

test("CH-2: a post that mentions no agent has no route line and its header names no one", async (t) => {
  const { ui } = await chatRoom(t, [
    channelPost("quiet", "heads up: rotating the staging keys at 3pm", {
      state: "unrouted",
      to: [],
    }),
  ]);
  const row = ui.page.locator('main [data-message-id="quiet"]');
  await row.waitFor();
  assert.equal(await row.locator(".route").count(), 0);
  assert.equal(await row.locator(".msg-head .to").count(), 0);
  assert.equal(await row.locator(".receipt").count(), 0);
});

test("CH-2: routing reads Checking who this is for, then a live routed frame fills in the same row", async (t) => {
  const post = (channel, legs = []) =>
    channelPost(
      "p1",
      "can @claude check why the mac build fails on main?",
      channel,
      {
        legs,
      },
    );
  const value = team(1, "alpha");
  value.frame.messages = [post({ state: "routing" })];
  let client;
  const ui = await open(t, (connected) => {
    client = connected;
    replay(connected, [value]);
  });
  await ui.live();
  const { route, parts, icon } = routeOf(ui, "p1");
  await route.waitFor();
  assert.deepEqual(await parts(), ["Checking who this is for…"]);
  assert.deepEqual(await icon(), [iconShape.clock]);
  await ui.page.locator('main [data-message-id="p1"]').evaluate((row) => {
    row.dataset.seen = "yes";
  });
  value.frame.messages = [
    post(routed(), [
      { to: "claude", stage: { stage: "working", acked_at_ms: 1 } },
    ]),
  ];
  client.send("space", value);
  await ui.page.getByText("build question", { exact: true }).waitFor();
  assert.deepEqual(await parts(), [
    "@claude",
    "build question",
    "Working on it",
  ]);
  assert.deepEqual(await icon(), [iconShape.arrow]);
  assert.equal(await route.getAttribute("title"), "Picked by claude-haiku");
  assert.equal(
    await ui.page
      .locator('main [data-message-id="p1"]')
      .getAttribute("data-seen"),
    "yes",
    "the row updated in place",
  );
  const name = route.getByRole("button", { name: "@claude", exact: true });
  assert.equal(
    await name.evaluate((element) => getComputedStyle(element).fontWeight),
    "600",
  );
  assert.equal(
    await name.evaluate((element) => getComputedStyle(element).color),
    await tokenColor(ui.page, "--text"),
    "names read in --text",
  );
  assert.equal(
    await route.evaluate((element) => getComputedStyle(element).fontSize),
    "12px",
  );
});

async function remountProbe(ui, id) {
  const row = await ui.page.$(`main [data-message-id="${id}"]`);
  const route = await ui.page.$(`main [data-message-id="${id}"] .route`);
  return () =>
    ui.page.evaluate(
      ([row, route, id]) => {
        const now = document.querySelector(`main [data-message-id="${id}"]`);
        return [
          now === row,
          now?.querySelector(".route") === route,
          now?.classList.contains("arrive"),
          now?.getAnimations({ subtree: true }).length,
        ];
      },
      [row, route, id],
    );
}

test("CH-fix-2 item 2: a post reads Sending… until the server answers, then Checking who this is for… until a frame routes it in place", async (t) => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const { ui, value } = await chatRoom(t, [], {
    onPost: async () => {
      await gate;
      return { status: 200 };
    },
  });
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  await box.fill("can @claude check why the mac build fails on main?");
  await box.press("Enter");
  const pending = ui.page.locator("main article.pending");
  await pending.waitFor();
  assert.equal(await pending.locator(".receipt").textContent(), "Sending…");
  release();
  await pending.locator(".receipt").getByText("Sent").waitFor();
  const { request_id } = JSON.parse(ui.posts.attempts[0].raw);
  const committed = (channel, legs) => ({
    ...ui.posts.postedRow(request_id, Date.now() - 60_000),
    kind: "mention",
    legs,
    channel: { mentioned: ["claude"], ...channel },
  });
  value.frame.messages = [committed({ state: "routing" }, [])];
  ui.connections[0].send("space", value);
  await pending.waitFor({ state: "detached" });
  const { route, parts, icon } = routeOf(ui, "posted-1");
  await route.waitFor();
  assert.deepEqual(await parts(), ["Checking who this is for…"]);
  assert.deepEqual(await icon(), [iconShape.clock]);
  assert.equal(await route.getByRole("button").count(), 0, "no offers yet");
  const same = await remountProbe(ui, "posted-1");
  value.frame.messages = [
    committed(routed(), [{ to: "claude", stage: { stage: "delivered" } }]),
  ];
  ui.connections[0].send("space", value);
  await route.getByText("build question", { exact: true }).waitFor();
  assert.deepEqual(await parts(), ["@claude", "build question", "Delivered"]);
  assert.deepEqual(await icon(), [iconShape.arrow]);
  assert.deepEqual(
    await same(),
    [true, true, false, 0],
    "the same row and line update with no arrival",
  );
});

for (const [state, text, expected] of [
  [
    "unrouted",
    "nice catch by @codex on the cache",
    ["Mentioned @codex, not sent", "Send anyway"],
  ],
  [
    "router_failed",
    "can @codex look at the flaky test?",
    ["Couldn't check who this is for", "Send to @codex"],
  ],
])
  test(`CH-fix-2 item 2: a routing post that turns ${state} offers to send only once it leaves routing`, async (t) => {
    const { ui, value } = await chatRoom(t, [
      channelPost("p1", text, { state: "routing" }),
    ]);
    const { route, parts } = routeOf(ui, "p1");
    await route.waitFor();
    assert.deepEqual(await parts(), ["Checking who this is for…"]);
    assert.equal(await route.getByRole("button").count(), 0, "no offers yet");
    const same = await remountProbe(ui, "p1");
    value.frame.messages = [channelPost("p1", text, { state })];
    ui.connections[0].send("space", value);
    await route.getByRole("button", { name: "Send this to @codex" }).waitFor();
    assert.deepEqual(await parts(), expected);
    assert.deepEqual(await same(), [true, true, false, 0]);
  });

test("CH-2: a routed post offers Send anyway for the mentioned agent it did not reach", async (t) => {
  const { ui } = await chatRoom(t, [
    channelPost(
      "p1",
      "thanks @codex, can @claude check the mac build?",
      routed(),
      {
        legs: [{ to: "claude", stage: { stage: "delivered" } }],
      },
    ),
  ]);
  const { route, parts, icon } = routeOf(ui, "p1");
  await route.waitFor();
  assert.deepEqual(await parts(), [
    "@claude",
    "build question",
    "Delivered",
    "Mentioned @codex, not sent",
    "Send anyway",
  ]);
  assert.deepEqual(await icon(), [iconShape.arrow]);
  const offer = route.getByRole("button", { name: "Send this to @codex" });
  assert.equal(await offer.textContent(), "Send anyway");
  assert.equal(
    await offer.evaluate((element) => getComputedStyle(element).color),
    await tokenColor(ui.page, "--accent-text"),
  );
});

test("CH-2: an unrouted post that mentions an agent says it was not sent, with the inbox and Send anyway", async (t) => {
  const { ui } = await chatRoom(t, [
    channelPost("p1", "nice catch by @codex on the cache", {
      state: "unrouted",
      to: [],
    }),
  ]);
  const { route, parts, icon } = routeOf(ui, "p1");
  await route.waitFor();
  assert.deepEqual(await parts(), [
    "Mentioned @codex, not sent",
    "Send anyway",
  ]);
  assert.deepEqual(await icon(), [iconShape.inbox]);
  assert.equal(await route.getAttribute("title"), null);
  await route.getByRole("button", { name: "Send this to @codex" }).waitFor();
});

test("CH-2: router_failed reads in --failed-text, explains itself, and offers Send to each agent", async (t) => {
  const { ui } = await chatRoom(t, [
    channelPost("p1", "can @codex and @claude look at the flaky test?", {
      state: "router_failed",
    }),
  ]);
  const { route, parts, icon } = routeOf(ui, "p1");
  await route.waitFor();
  assert.deepEqual(await parts(), [
    "Couldn't check who this is for",
    "Send to @codex, @claude",
  ]);
  assert.deepEqual(await icon(), [iconShape.alert]);
  assert.equal(
    await route.getAttribute("title"),
    "Namedrop couldn't reach its router. The post is saved, and no one got it.",
  );
  const failed = await tokenColor(ui.page, "--failed-text");
  assert.equal(
    await route
      .locator(".route-state")
      .evaluate((element) => getComputedStyle(element).color),
    failed,
  );
  assert.equal(
    await route
      .locator(".route-icon")
      .evaluate((element) => getComputedStyle(element).color),
    failed,
  );
  for (const alias of ["@codex", "@claude"])
    assert.equal(
      await route
        .getByRole("button", { name: `Send this to ${alias}` })
        .textContent(),
      alias,
    );
});

test("CH-2: replies read as their own rows in time order, with a reference above a reply whose parent is not just above it", async (t) => {
  const at = Date.now() - 600_000;
  const { ui } = await chatRoom(t, [
    channelPost(
      "p",
      "can @claude check why the mac build fails on main?",
      routed(),
      {
        at_ms: at,
        legs: [{ to: "claude", stage: { stage: "working", acked_at_ms: 1 } }],
      },
    ),
    answer(
      "r1",
      "p",
      "p",
      "claude",
      "The arm64 link step fails. @codex, is the CI cache stale?\nMore detail here.",
      {
        at_ms: at + 60_000,
        legs: [
          { to: "human", stage: { stage: "delivered" } },
          { to: "codex", stage: { stage: "working", acked_at_ms: 1 } },
        ],
        woke: ["codex"],
      },
    ),
    message("other", "@shell ls", {
      thread_id: "t-other",
      at_ms: at + 90_000,
      legs: [{ to: "shell", stage: { stage: "queued" } }],
    }),
    answer(
      "r2",
      "r1",
      "p",
      "codex",
      "Yes. I cleared it, and the build passes now.",
      {
        at_ms: at + 120_000,
        legs: [{ to: "human", stage: { stage: "delivered" } }],
      },
    ),
    answer("r3", "gone", "p", "claude", "Following up on an older point.", {
      at_ms: at + 150_000,
    }),
  ]);
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.locator('[data-message-id="r3"]').waitFor();
  assert.deepEqual(
    await timeline
      .locator("article[data-message-id]")
      .evaluateAll((rows) => rows.map((row) => row.dataset.messageId)),
    ["p", "r1", "other", "r2", "r3"],
  );
  assert.equal(
    await timeline.locator(".reply-preview").count(),
    0,
    "no reply preview",
  );
  assert.deepEqual(await routeOf(ui, "p").parts(), [
    "@claude",
    "build question",
    "3 replies",
  ]);
  assert.deepEqual(await routeOf(ui, "r1").parts(), ["@codex", "pulled in"]);
  assert.equal(
    await timeline.locator('[data-message-id="r1"] .msg-head .to').count(),
    0,
  );
  assert.equal(
    await timeline.locator('[data-message-id="r1"] .reply-ref').count(),
    0,
  );
  assert.equal(
    await routeOf(ui, "r2").route.count(),
    0,
    "a reply to the operator only",
  );
  const reference = timeline.getByRole("button", {
    name: "Reply to @claude: The arm64 link step fails. @codex, is the CI cache stale? Open thread",
  });
  assert.equal(
    await reference.textContent(),
    "@claude:The arm64 link step fails. @codex, is the CI cache stale?",
  );
  const [referenceBox, headBox] = await Promise.all([
    reference.boundingBox(),
    timeline.locator('[data-message-id="r2"] .msg-head').boundingBox(),
  ]);
  assert.ok(referenceBox.y < headBox.y, "the reference sits above the header");
  assert.ok(
    Math.abs(referenceBox.x - headBox.x) < 1,
    "aligned with the text column",
  );
  await timeline
    .getByRole("button", { name: "Reply to an earlier message. Open thread" })
    .waitFor();
  await timeline
    .getByRole("button", { name: "3 replies. Open thread" })
    .waitFor();
  await reference.click();
  const pane = ui.page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  await pane.locator('[data-message-id="p"] .route').waitFor();
  assert.equal(
    await pane.locator(".reply-ref").count(),
    0,
    "no references in the pane",
  );
  assert.equal(
    await pane.locator(".composer-to").textContent(),
    "To @claude, @codex",
  );
  const box = pane.getByRole("combobox", { name: "Reply to @claude, @codex" });
  await box.pressSequentially("thanks both");
  assert.equal(
    await pane.locator(".composer-status").textContent(),
    "Enter to send · Shift+Enter for a new line",
  );
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the reply");
  const posted = JSON.parse(ui.posts.attempts[0].raw);
  assert.equal(posted.source, "thanks both", "sent as typed");
  assert.equal(posted.in_reply_to, "r3");
  await ui.page.keyboard.press("Escape");
  await pane.waitFor({ state: "detached" });
  assert.equal(
    await ui.page.evaluate(() =>
      document.activeElement?.getAttribute("aria-label"),
    ),
    "Reply to @claude: The arm64 link step fails. @codex, is the CI cache stale? Open thread",
    "focus returns to the reference",
  );
});

for (const woke of [["claude"], []])
  test(`CH-fix-2 item 5: the pull-in loop reads naturally, and the asker hearing back is never pulled in (woke ${JSON.stringify(woke)})`, async (t) => {
    const at = Date.now() - 600_000;
    const { ui } = await chatRoom(t, [
      channelPost("p", "can @claude find out what 6 times 7 is?", routed(""), {
        at_ms: at,
        legs: [{ to: "claude", stage: done("replied") }],
      }),
      answer(
        "ask",
        "p",
        "p",
        "claude",
        "Asking now: @codex what is 6 times 7?",
        {
          at_ms: at + 60_000,
          legs: [
            { to: "human", stage: { stage: "delivered" } },
            { to: "codex", stage: done("replied") },
          ],
          woke: ["codex"],
        },
      ),
      channelPost(
        "room",
        "heads up: the staging keys rotate at 3pm",
        {
          state: "unrouted",
          to: [],
        },
        { at_ms: at + 90_000, thread_id: "room" },
      ),
      answer("answer", "ask", "p", "codex", "42", {
        at_ms: at + 120_000,
        legs: [
          { to: "human", stage: { stage: "delivered" } },
          { to: "claude", stage: done("replied") },
        ],
        woke,
      }),
      answer("report", "answer", "p", "claude", "It's 42.", {
        at_ms: at + 180_000,
        legs: [{ to: "human", stage: { stage: "delivered" } }],
      }),
    ]);
    const timeline = ui.page.getByRole("region", { name: "Message history" });
    await timeline.locator('[data-message-id="report"]').waitFor();
    assert.deepEqual(await routeOf(ui, "ask").parts(), ["@codex", "pulled in"]);
    assert.deepEqual(
      await routeOf(ui, "answer").parts(),
      ["@claude"],
      "the asker hears back as a plain recipient",
    );
    await timeline
      .locator('[data-message-id="answer"]')
      .getByRole("button", {
        name: "Reply to @claude: Asking now: @codex what is 6 times 7? Open thread",
      })
      .waitFor();
    assert.equal(
      await routeOf(ui, "report").route.count(),
      0,
      "the report back to the room has no route line",
    );
    assert.equal(
      await timeline.getByText("pulled in", { exact: true }).count(),
      1,
    );
  });

const threadPane = (ui) =>
  ui.page.getByRole("complementary", { name: "Thread", exact: true });
function held() {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  return { gate, release: () => release() };
}
const deliverPath = (id) => `/w/${key(1)}/messages/${id}/deliver`;
function reached(post, ...to) {
  return {
    ...post,
    channel: { ...post.channel, to },
    legs: to.map((id) => ({ to: id, stage: { stage: "delivered" } })),
  };
}
const offerState = (button) =>
  button.evaluate((element) => ({
    text: element.textContent,
    disabled: element.getAttribute("aria-disabled"),
    title: element.getAttribute("title"),
    focused: document.activeElement === element,
    connected: element.isConnected,
  }));
async function offerReads(button, text) {
  await eventually(
    async () => (await offerState(button)).text === text,
    `the offer to read ${text}`,
  );
  return offerState(button);
}
const shownText = (locator) =>
  locator.evaluate((root) => {
    const divider = (element, which) =>
      getComputedStyle(element, which).content.startsWith('"·"') ? " · " : "";
    const read = (node) =>
      node.nodeType === Node.TEXT_NODE
        ? node.textContent
        : node.nodeType === Node.ELEMENT_NODE
          ? divider(node, "::before") +
            [...node.childNodes].map(read).join("") +
            divider(node, "::after")
          : "";
    return read(root).replace(/\s+/g, " ").trim();
  });
const focusedOffer = (page) =>
  page.evaluate(() => ({
    offer: document.activeElement?.getAttribute("data-offer") ?? null,
    text: document.activeElement?.textContent ?? null,
  }));
async function settled(read, expected) {
  const deadline = Date.now() + 2000;
  let value = await read();
  while (!isDeepStrictEqual(value, expected) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 10));
    value = await read();
  }
  return value;
}
async function readsAs(locator, text) {
  assert.equal(await settled(() => shownText(locator), text), text);
}
async function focusOn(page, offer, text) {
  assert.deepEqual(await settled(() => focusedOffer(page), { offer, text }), {
    offer,
    text,
  });
}
const pressedOffer = (scope, id, alias) =>
  scope.locator(`[data-offer="${id} ${alias}"]`).elementHandle();
const unsent = () =>
  channelPost("p1", "nice catch by @codex on the cache", {
    state: "unrouted",
    to: [],
  });

test("CH-fix-3 item 1: Send anyway delivers the post itself, @codex leaves the not-sent part for its own Sending to @codex… then Sent to @codex with focus on it, and the frame's leg replaces it", async (t) => {
  const answered = held();
  const post = unsent();
  const { ui, value } = await chatRoom(t, [post], {
    onDeliver: async () => {
      await answered.gate;
      return { status: 200 };
    },
  });
  const { route, parts } = routeOf(ui, "p1");
  await route.waitFor();
  assert.equal(
    await shownText(route),
    "Mentioned @codex, not sent · Send anyway",
  );
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await until(() => ui.posts.deliveries.length === 1, "the deliver POST");
  const [{ method, path, csrf, type, body }] = ui.posts.deliveries;
  assert.deepEqual(
    { method, path, csrf, type, body },
    {
      method: "POST",
      path: deliverPath("p1"),
      csrf: "csrf-1",
      type: "application/json",
      body: '{"to":["codex"]}',
    },
  );
  await readsAs(route, "Sending to @codex…");
  await focusOn(ui.page, "p1 @codex", "Sending to @codex…");
  const button = await pressedOffer(route, "p1", "@codex");
  assert.deepEqual(await offerState(button), {
    text: "Sending to @codex…",
    disabled: "true",
    title: null,
    focused: true,
    connected: true,
  });
  assert.equal(
    await route.getByRole("status").textContent(),
    "Sending to @codex…",
  );
  assert.equal(await threadPane(ui).count(), 0, "no thread opens");
  assert.equal(
    await ui.page.locator("main textarea").inputValue(),
    "",
    "nothing is typed in the channel box",
  );
  answered.release();
  assert.deepEqual(await offerReads(button, "Sent to @codex"), {
    text: "Sent to @codex",
    disabled: "true",
    title: null,
    focused: true,
    connected: true,
  });
  await readsAs(route, "Sent to @codex");
  assert.equal(await route.getByRole("status").textContent(), "Sent to @codex");
  await button.click({ force: true });
  value.frame.messages = [reached(post, "codex")];
  ui.connections[0].send("space", value);
  await eventually(
    async () => !(await button.evaluate((element) => element.isConnected)),
    "the part to leave once the post reached @codex",
  );
  assert.deepEqual(await parts(), ["@codex", "Delivered"]);
  assert.equal(
    await ui.page.locator("main article[data-message-id]").count(),
    1,
    "the room shows the post once",
  );
  assert.equal(ui.posts.deliveries.length, 1, "Sent never sends again");
  assert.equal(ui.posts.attempts.length, 0, "no message is posted");
});

test("DESIGN-R2 B4: the deliver answer's channel object names @codex on the line right after the answer, with no frame", async (t) => {
  const post = unsent();
  const { ui } = await chatRoom(t, [post], {
    onDeliver: () => ({ channel: { ...post.channel, to: ["codex"] } }),
  });
  const { route, parts, icon } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await readsAs(route, "@codex · Queued");
  assert.deepEqual(await parts(), ["@codex", "Queued"]);
  assert.deepEqual(await icon(), [iconShape.arrow]);
  assert.equal(
    await route.locator("[data-offer]").count(),
    0,
    "no offer and no Sent part",
  );
  assert.equal(ui.posts.deliveries.length, 1);
  assert.equal(ui.connections.length, 1, "no new frame or connection");
});

test("CH-fix-3 item 1: a refused delivery reads Couldn't send to @codex · Try again with focus on Try again, its title is the server's message, and Try again delivers", async (t) => {
  const refusal = "@codex is not an agent in this workspace.";
  const outcomes = [{ status: 400, error: refusal }, { status: 200 }];
  const { ui } = await chatRoom(t, [unsent()], {
    onDeliver: () => outcomes.shift(),
  });
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await readsAs(route, "Couldn't send to @codex · Try again");
  await focusOn(ui.page, "p1 @codex", "Try again");
  const button = await pressedOffer(route, "p1", "@codex");
  assert.deepEqual(await offerState(button), {
    text: "Try again",
    disabled: null,
    title: refusal,
    focused: true,
    connected: true,
  });
  assert.equal(
    await shownText(route.getByRole("status")),
    "Couldn't send to @codex · Try again",
  );
  assert.equal(
    await route
      .getByText("Couldn't send to @codex", { exact: true })
      .evaluate((element) => getComputedStyle(element).color),
    await tokenColor(ui.page, "--failed-text"),
  );
  await button.click();
  assert.equal((await offerReads(button, "Sent to @codex")).focused, true);
  await readsAs(route, "Sent to @codex");
  assert.deepEqual(
    ui.posts.deliveries.map(({ path, body, status }) => [path, body, status]),
    [
      [deliverPath("p1"), '{"to":["codex"]}', 400],
      [deliverPath("p1"), '{"to":["codex"]}', 200],
    ],
  );
});

test("CH-fix-3 item 1: Try again's title says Couldn't reach Namedrop. when the request never lands, and the HTTP status when a refusal has no message", async (t) => {
  let outcome = { drop: true };
  const { ui } = await chatRoom(t, [unsent()], { onDeliver: () => outcome });
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await focusOn(ui.page, "p1 @codex", "Try again");
  const button = await pressedOffer(route, "p1", "@codex");
  assert.equal((await offerState(button)).title, "Couldn't reach Namedrop.");
  outcome = { status: 500 };
  await button.click();
  await eventually(
    async () => (await offerState(button)).title === "HTTP 500",
    "the status in the title",
  );
  assert.equal(
    await shownText(route.getByRole("status")),
    "Couldn't send to @codex · Try again",
  );
});

test("CH-fix-4: a 200 that did not deliver to @codex reads Couldn't send to @codex · Try again, says the name may belong to a different agent now, and Try again sends it", async (t) => {
  const outcomes = [{ delivered: [] }, { delivered: ["codex"] }];
  const { ui } = await chatRoom(t, [unsent()], {
    onDeliver: () => outcomes.shift(),
  });
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await readsAs(route, "Couldn't send to @codex · Try again");
  await focusOn(ui.page, "p1 @codex", "Try again");
  const button = await pressedOffer(route, "p1", "@codex");
  assert.deepEqual(await offerState(button), {
    text: "Try again",
    disabled: null,
    title:
      "Namedrop didn't send it. The name may belong to a different agent now.",
    focused: true,
    connected: true,
  });
  assert.equal(
    await shownText(route.getByRole("status")),
    "Couldn't send to @codex · Try again",
  );
  await button.click();
  assert.equal((await offerReads(button, "Sent to @codex")).focused, true);
  await readsAs(route, "Sent to @codex");
  assert.deepEqual(
    ui.posts.deliveries.map(({ path, body, status }) => [path, body, status]),
    [
      [deliverPath("p1"), '{"to":["codex"]}', 200],
      [deliverPath("p1"), '{"to":["codex"]}', 200],
    ],
  );
});

test("DESIGN-R2 B6: a 200 whose delivered list is missing or malformed reads Couldn't send to @codex · Try again, Namedrop didn't confirm it sent this., and an empty 200 body reads Sent to @codex", async (t) => {
  const malformed = [undefined, null, "codex", ["codex", 7]];
  const outcomes = [
    ...malformed.map((delivered) => ({ delivered })),
    { raw: "" },
  ];
  const { ui } = await chatRoom(t, [unsent()], {
    onDeliver: () => outcomes.shift(),
  });
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  for (const [index, delivered] of malformed.entries()) {
    await until(
      () => ui.posts.deliveries[index]?.status === 200,
      `answer ${index + 1}`,
    );
    await readsAs(route, "Couldn't send to @codex · Try again");
    const button = await pressedOffer(route, "p1", "@codex");
    assert.deepEqual(
      await offerState(button),
      {
        text: "Try again",
        disabled: null,
        title: "Namedrop didn't confirm it sent this.",
        focused: true,
        connected: true,
      },
      JSON.stringify(delivered),
    );
    await button.click();
  }
  await readsAs(route, "Sent to @codex");
  assert.deepEqual(
    ui.posts.deliveries.map(({ body, status }) => [body, status]),
    Array.from({ length: 5 }, () => ['{"to":["codex"]}', 200]),
  );
});

test("DESIGN-R2 B5: a 200 whose body stalls reads Sent to @codex once the 5 s body wait ends, and the page closes that connection", async (t) => {
  const { ui } = await chatRoom(t, [unsent()], {
    onDeliver: () => ({ stall: true }),
  });
  await ui.page.clock.install();
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await until(() => ui.posts.deliveries[0]?.status === 200, "the 200 headers");
  await readsAs(route, "Sending to @codex…");
  await ui.page.clock.runFor(5_000);
  await readsAs(route, "Sent to @codex");
  await until(
    () => ui.posts.deliveries[0].closed,
    "the server to see the connection close",
  );
});

test("CH-fix-3 item 1: Send to after router_failed moves each pressed agent to its own part after the ones still not sent", async (t) => {
  const refusal = "@claude is not an agent in this workspace.";
  const codexAnswer = held();
  const { ui } = await chatRoom(
    t,
    [
      channelPost("p1", "can @codex and @claude look at the flaky test?", {
        state: "router_failed",
      }),
    ],
    {
      onDeliver: async ({ to: [alias] }) => {
        if (alias !== "codex") return { status: 400, error: refusal };
        await codexAnswer.gate;
        return { status: 200 };
      },
    },
  );
  const { route } = routeOf(ui, "p1");
  await route.waitFor();
  assert.equal(
    await shownText(route),
    "Couldn't check who this is for · Send to @codex, @claude",
  );
  await route.getByRole("button", { name: "Send this to @codex" }).click();
  await readsAs(
    route,
    "Couldn't check who this is for · Send to @claude · Sending to @codex…",
  );
  await focusOn(ui.page, "p1 @codex", "Sending to @codex…");
  const codex = await pressedOffer(route, "p1", "@codex");
  codexAnswer.release();
  assert.equal((await offerReads(codex, "Sent to @codex")).focused, true);
  await readsAs(
    route,
    "Couldn't check who this is for · Send to @claude · Sent to @codex",
  );
  await route.getByRole("button", { name: "Send this to @claude" }).click();
  await readsAs(
    route,
    "Couldn't check who this is for · Sent to @codex · Couldn't send to @claude · Try again",
  );
  await focusOn(ui.page, "p1 @claude", "Try again");
  const claude = await pressedOffer(route, "p1", "@claude");
  assert.deepEqual(await offerState(claude), {
    text: "Try again",
    disabled: null,
    title: refusal,
    focused: true,
    connected: true,
  });
  assert.equal((await offerState(codex)).text, "Sent to @codex");
  assert.deepEqual(
    ui.posts.deliveries.map(({ path, body }) => [path, JSON.parse(body)]),
    [
      [deliverPath("p1"), { to: ["codex"] }],
      [deliverPath("p1"), { to: ["claude"] }],
    ],
  );
});

test("CH-fix-3 item 1: Send anyway in the thread pane delivers the post, focus moves to the pane's part, and the timeline shows the same state", async (t) => {
  const at = Date.now() - 600_000;
  const post = channelPost(
    "p1",
    "nice catch by @codex on the cache",
    { state: "unrouted", to: [] },
    { at_ms: at },
  );
  const reply = answer("r1", "p1", "p1", "claude", "agreed, nice one", {
    at_ms: at + 60_000,
    legs: [{ to: "human", stage: { stage: "delivered" } }],
  });
  const answered = held();
  const { ui } = await chatRoom(t, [post, reply], {
    onDeliver: async () => {
      await answered.gate;
      return { status: 200 };
    },
  });
  await ui.page
    .locator("main")
    .getByRole("button", { name: "1 reply. Open thread" })
    .click();
  const pane = threadPane(ui);
  const box = pane.getByRole("combobox");
  await box.waitFor();
  await pane.getByRole("button", { name: "Send this to @codex" }).click();
  await until(() => ui.posts.deliveries.length === 1, "the deliver POST");
  assert.equal(ui.posts.deliveries[0].path, deliverPath("p1"));
  const inTimeline = routeOf(ui, "p1").route;
  await readsAs(inTimeline, "Sending to @codex… · 1 reply");
  await readsAs(pane.locator(".route"), "Sending to @codex…");
  const button = await pressedOffer(pane, "p1", "@codex");
  assert.equal((await offerState(button)).focused, true);
  answered.release();
  assert.equal((await offerReads(button, "Sent to @codex")).focused, true);
  await readsAs(inTimeline, "Sent to @codex · 1 reply");
  assert.equal(await box.inputValue(), "", "nothing is typed in the reply box");
  assert.equal(ui.posts.attempts.length, 0, "no reply is posted");
});

test("CH-fix-3 item 1: Send anyway in a Needs-you conversation pane delivers the post and leaves the reply box alone", async (t) => {
  const value = frame(1, "alpha", "needs_you");
  value.frame.participants = people();
  const post = channelPost(
    "far-post",
    "the deploy notes @codex wrote last week were clear",
    { state: "unrouted", to: [] },
  );
  const conversation = { thread_id: "far-post", message_id: "far-post" };
  value.frame.attention = [
    {
      source_kind: "delivery_attempt",
      summary: { text: "A response is needed.", truncated: false },
      opened_at_ms: 4,
      subject_alias: null,
      message_id: "far-post",
      conversation,
      coordination_id: null,
      response: { kind: "reply", ...conversation, address_id: "codex" },
    },
  ];
  const ui = await open(
    t,
    (client) => replay(client, [value]),
    () => [],
    {
      onContext: () => ({
        thread_id: "far-post",
        message_count: 1,
        earlier_omitted: false,
        messages: [post],
      }),
    },
  );
  await ui.live();
  await ui.page
    .getByRole("button", { name: "Show message from @codex", exact: true })
    .click();
  const pane = threadPane(ui);
  const reply = pane.getByRole("combobox");
  await reply.waitFor();
  const before = await reply.inputValue();
  await pane.getByRole("button", { name: "Send this to @codex" }).click();
  await readsAs(pane.locator(".route"), "Sent to @codex");
  await focusOn(ui.page, "far-post @codex", "Sent to @codex");
  assert.deepEqual(
    ui.posts.deliveries.map(({ path, body }) => [path, body]),
    [[deliverPath("far-post"), '{"to":["codex"]}']],
  );
  assert.equal(await reply.inputValue(), before, "the reply box is untouched");
  assert.equal(ui.posts.attempts.length, 0);
});

test("CH-fix-3 item 1: a double click on Send anyway sends one delivery", async (t) => {
  const answered = held();
  const { ui } = await chatRoom(t, [unsent()], {
    onDeliver: async () => {
      await answered.gate;
      return { status: 200 };
    },
  });
  const { route } = routeOf(ui, "p1");
  await route.getByRole("button", { name: "Send this to @codex" }).dblclick();
  await until(() => ui.posts.deliveries.length > 0, "the deliver POST");
  answered.release();
  await readsAs(route, "Sent to @codex");
  assert.equal(ui.posts.deliveries.length, 1);
});

test("thread reply: a name with no message after it stays in the box and says what is missing", async (t) => {
  const at = Date.now() - 600_000;
  const post = channelPost(
    "p1",
    "nice catch by @codex on the cache",
    { state: "unrouted", to: [] },
    { at_ms: at },
  );
  const reply = answer("r1", "p1", "p1", "claude", "agreed, nice one", {
    at_ms: at + 60_000,
    legs: [{ to: "human", stage: { stage: "delivered" } }],
  });
  const { ui } = await chatRoom(t, [post, reply]);
  await ui.page
    .locator("main")
    .getByRole("button", { name: "1 reply. Open thread" })
    .click();
  const pane = threadPane(ui);
  const box = pane.getByRole("combobox");
  await box.fill("@codex ");
  await box.press("Enter");
  assert.equal(
    await pane.locator(".composer-status").textContent(),
    "Add a message after @codex.",
  );
  assert.equal(
    await pane.getByRole("button", { name: "Send", exact: true }).isDisabled(),
    true,
  );
  assert.equal(ui.posts.attempts.length, 0, "a name alone never posts");
  await box.pressSequentially("hi");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the reply");
  assert.equal(JSON.parse(ui.posts.attempts[0].raw).source, "@codex hi");
});

test("CH-2: the channel composer says where Enter sends and posts plain text", async (t) => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const { ui } = await chatRoom(t, [], {
    onPost: async () => {
      await gate;
      return { status: 200 };
    },
  });
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  const status = ui.page.locator("main .composer-status");
  await box.click();
  const hints = [
    ["", "Enter posts to the channel · Mention @name to bring someone in"],
    [
      "hello room",
      "Enter posts to the channel · Mention @name to bring someone in",
    ],
    [
      "ask @shell",
      "Enter posts to the channel · Mention @name to bring someone in",
    ],
    [
      "can @codex look",
      "Enter posts to the channel · Namedrop checks if it's for @codex",
    ],
    [
      "can @codex and @claude look",
      "Enter posts to the channel · Namedrop checks if it's for @codex, @claude",
    ],
    ["@codex hi", "Enter sends only to @codex"],
    ["#alpha @codex @shell hi", "Enter sends only to @codex, @shell"],
  ];
  for (const [text, hint] of hints) {
    await box.fill(text);
    assert.equal(await status.textContent(), hint, JSON.stringify(text));
  }
  await box.fill("can @codex look at the build?");
  await box.press("Enter");
  const pending = ui.page.locator("main article.pending");
  await pending.waitFor();
  assert.equal(await pending.locator(".receipt").textContent(), "Sending…");
  assert.deepEqual(
    await pending
      .locator(".receipt svg path")
      .evaluateAll((paths) => paths.map((path) => path.getAttribute("d"))),
    [iconShape.clock],
  );
  release();
  await pending.locator(".receipt").getByText("Sent").waitFor();
  const first = JSON.parse(ui.posts.attempts[0].raw);
  assert.equal(first.source, "can @codex look at the build?");
  assert.equal("in_reply_to" in first, false);
  await box.fill("hello room");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 2, "the plain post");
  assert.equal(JSON.parse(ui.posts.attempts[1].raw).source, "hello room");
});

test("DESIGN-R2 B1: the channel hint names a capitalized or unknown leading name as you type, and Enter still sends it for the server to refuse", async (t) => {
  const { ui } = await chatRoom(t, [], {
    onPost: ({ source }) => ({
      status: 400,
      error: `unknown Mention: ${source.split(" ")[0]}`,
    }),
  });
  const box = ui.page.getByRole("combobox", { name: "Message #alpha" });
  const status = ui.page.locator("main .composer-status");
  await box.click();
  const hints = [];
  for (const part of ["@Codex", " quick check"]) {
    await box.pressSequentially(part);
    hints.push(await status.textContent());
  }
  await box.fill("");
  for (const part of ["@Nobody", " hi there"]) {
    await box.pressSequentially(part);
    hints.push(await status.textContent());
  }
  assert.deepEqual(hints, [
    "Names are lowercase: @codex",
    "Names are lowercase: @codex",
    "@Nobody isn't in this channel",
    "@Nobody isn't in this channel",
  ]);
  assert.equal(ui.posts.attempts.length, 0, "nothing is sent before Enter");
  await box.press("Enter");
  await until(() => ui.posts.attempts.length === 1, "the post");
  assert.equal(JSON.parse(ui.posts.attempts[0].raw).source, "@Nobody hi there");
  await status.getByText("unknown Mention: @Nobody").waitFor();
  assert.equal(await box.inputValue(), "@Nobody hi there", "the text stays");
});

test("CH-2: an agent conversation shows every message flat, with header recipients and receipts", async (t) => {
  const at = Date.now() - 600_000;
  const { ui } = await chatRoom(t, [
    message("ask", "please reply with the single word pong.", {
      thread_id: "t1",
      kind: "handoff",
      author: "claude",
      at_ms: at,
      legs: [{ to: "codex", stage: done() }],
    }),
    answer("pong", "ask", "t1", "codex", "pong", {
      at_ms: at + 60_000,
      legs: [{ to: "claude", stage: { stage: "delivered" } }],
    }),
  ]);
  const timeline = ui.page.getByRole("region", { name: "Message history" });
  await timeline.locator('[data-message-id="pong"]').waitFor();
  assert.equal(
    await timeline
      .locator('[data-message-id="pong"] .msg-head .to')
      .textContent(),
    " to @claude",
  );
  assert.equal(
    await timeline.locator('[data-message-id="pong"] .receipt').textContent(),
    "Delivered",
  );
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .receipt').count(),
    0,
    "@codex answered, so the ask's receipts hide",
  );
  assert.equal(
    await timeline.locator('[data-message-id="pong"] .route').count(),
    0,
    "a reply in an agent conversation has no route line",
  );
  assert.equal(
    await timeline.locator('[data-message-id="ask"] .route').textContent(),
    "1 reply",
    "the first message links its replies the way a channel post does",
  );
  assert.equal(
    await timeline.locator(".reply-preview").count(),
    0,
    "no reply preview box",
  );
  await timeline.getByRole("button", { name: "1 reply. Open thread" }).click();
  await ui.page
    .getByRole("complementary", { name: "Thread", exact: true })
    .getByRole("combobox", { name: "Reply to @claude, @codex" })
    .waitFor();
});
