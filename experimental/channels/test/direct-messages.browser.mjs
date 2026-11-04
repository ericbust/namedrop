import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname } from "node:path";
import { test } from "node:test";
import { chromium } from "playwright-core";

const key = (n) => n.toString(16).padStart(20, "0");
const json = (res, value, status = 200) => {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(value));
};
const message = (id, body, author = "human") => ({
  message_id: id,
  thread_id: "private-thread",
  in_reply_to: null,
  at_ms: Date.now(),
  author,
  kind: "mention",
  body: { text: body, truncated: false },
  legs: [],
});
const peer = (space, address, workspace, extra = {}) => ({
  space_key: key(space),
  address_id: address,
  alias: "claude",
  workspace_label: workspace,
  pane_label: { text: `Pane ${address}`, truncated: false },
  runtime: "claude-code",
  presence: "idle",
  closed: false,
  thread_id: null,
  last_activity_ms: null,
  last_message_id: null,
  last_agent_message_id: null,
  attention_count: 0,
  ...extra,
});

test("private DMs keep duplicate names, replies, drafts and unread separate from channels", {
  timeout: 60000,
}, async (t) => {
  const peers = [
    peer(1, "a", "alpha"),
    peer(2, "b", "beta"),
    peer(2, "c", "beta", { alias: "codex", runtime: "codex" }),
    peer(3, "closed", "archived", {
      closed: true,
      presence: "gone",
      thread_id: "closed-thread",
    }),
  ];
  const conversations = new Map(peers.map((peer) => [peer.address_id, []]));
  conversations.set("closed", [
    message("old-private", "Retained closed history", "closed"),
    ...Array.from({ length: 60 }, (_, index) =>
      message(`closed-${index}`, `Earlier private message ${index}`, "closed"),
    ),
  ]);
  const posts = [];
  const reads = [];
  const screenReads = [];
  const skillReads = [];
  let clients = [];
  let directoryReads = 0;
  let tokenReads = 0;
  let delayBeta = false;
  let delayOlder = false;
  let sendFailures = 1;
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
  const frames = [1, 2].map((n) => {
    const value = structuredClone(fixture);
    value.key = key(n);
    value.frame.space = {
      ...value.frame.space,
      key: key(n),
      display_name: n === 1 ? "alpha" : "beta",
      addressable_label: n === 1 ? "alpha" : "beta",
      not_addressable_reason: null,
    };
    value.frame.messages = [
      {
        ...message(`public-${n}`, `Public channel ${n}`),
        thread_id: `public-${n}`,
      },
    ];
    value.frame.threads = [];
    value.frame.attention = [];
    value.frame.health = { level: "calm" };
    return value;
  });
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    if (url.pathname === "/api/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-store",
      });
      res.write("retry: 1000\n\n");
      clients.push(res);
      req.on("close", () => {
        clients = clients.filter((client) => client !== res);
      });
      return;
    }
    if (url.pathname === "/api/session")
      return json(res, { csrf_token: `csrf-${++tokenReads}` });
    if (url.pathname === "/api/direct-messages") {
      directoryReads++;
      return json(res, { peers });
    }
    const direct = /^\/w\/([^/]+)\/direct-messages\/([^/]+)\/messages$/.exec(
      url.pathname,
    );
    if (direct) {
      const address = decodeURIComponent(direct[2]);
      reads.push({ space: direct[1], address });
      if (req.method === "POST") {
        let raw = "";
        for await (const chunk of req) raw += chunk;
        const body = JSON.parse(raw);
        posts.push({
          space: direct[1],
          address,
          body,
          csrf: req.headers["x-namedrop-csrf"],
        });
        if (sendFailures-- > 0)
          return json(res, { error: "Expired token" }, 403);
        const id = `private-${posts.length}`;
        const reply = message(
          `reply-${posts.length}`,
          `Private reply for ${address}`,
          address,
        );
        conversations.get(address).push(message(id, body.source), reply);
        const target = peers.find((peer) => peer.address_id === address);
        target.thread_id = "private-thread";
        target.last_activity_ms = Date.now();
        target.last_message_id = reply.message_id;
        target.last_agent_message_id = reply.message_id;
        return json(res, { message_id: id, thread_id: "private-thread" });
      }
      const all = conversations.get(address);
      const before = url.searchParams.get("before");
      const end =
        before === null
          ? all.length
          : all.findIndex((item) => item.message_id === before);
      const start = Math.max(0, end - 100);
      const body = {
        thread_id: all.length ? "private-thread" : null,
        messages: all.slice(start, end),
        earlier_omitted: start > 0,
      };
      if (before !== null && delayOlder)
        return setTimeout(() => json(res, body), 4000);
      if (address === "b" && delayBeta)
        return setTimeout(() => json(res, body), 1000);
      return json(res, body);
    }
    if (url.pathname.endsWith("/skills")) {
      skillReads.push(url.pathname);
      return json(res, {
        runtime: "claude",
        skills: [
          {
            name: "review",
            description: "Review privately",
            source: "project",
          },
        ],
      });
    }
    if (url.pathname.endsWith("/live")) return json(res, {}, 404);
    if (url.pathname.endsWith("/screen")) {
      screenReads.push(url.pathname);
      return json(res, { error: "Gone" }, 410);
    }
    try {
      const path = url.pathname.startsWith("/assets/")
        ? url.pathname.slice(1)
        : "index.html";
      const body = await readFile(new URL(`../dist/${path}`, import.meta.url));
      res.writeHead(200, {
        "Content-Type": {
          ".js": "text/javascript",
          ".css": "text/css",
          ".html": "text/html",
        }[extname(path)],
      });
      res.end(body);
    } catch {
      res.writeHead(404);
      res.end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const browser = await chromium.launch({
    executablePath: process.env.CHROME ?? "/usr/bin/chromium",
    headless: true,
    args: ["--no-sandbox"],
  });
  t.after(async () => {
    await browser.close();
    for (const client of clients) client.end();
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 900 },
  });
  page.setDefaultTimeout(8000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/w/${key(1)}`);
  const entries = page.locator("[data-dm-key]");
  await entries.first().waitFor();
  assert.equal(
    await entries.count(),
    3,
    "directory loads before channel discovery",
  );
  const a = page.locator(`[data-dm-key="${key(1)}/a"]`);
  const b = page.locator(`[data-dm-key="${key(2)}/b"]`);
  assert.match(
    await a.getAttribute("aria-label"),
    /@claude · #alpha · Pane a · idle/,
  );
  assert.match(await b.getAttribute("aria-label"), /@claude · #beta/);
  for (const client of clients) {
    for (const frame of frames)
      client.write(`event: space\ndata: ${JSON.stringify(frame)}\n\n`);
    client.write(
      `event: ready\ndata: ${JSON.stringify({ keys: frames.map((frame) => frame.key) })}\n\n`,
    );
  }
  await page.getByText("Public channel 1", { exact: true }).waitFor();
  const filter = page.getByRole("searchbox", {
    name: "Filter direct messages",
  });
  await filter.fill("claude beta");
  assert.equal(await entries.count(), 1);
  await filter.fill("");
  await a.click();
  const composer = page.getByRole("combobox", {
    name: "Message @claude privately",
  });
  await composer.fill("alpha draft");
  await b.click();
  await composer.fill("beta draft");
  await a.click();
  assert.equal(await composer.inputValue(), "alpha draft");
  await composer.fill("@cl");
  await page.getByRole("listbox", { name: "Members" }).waitFor();
  await composer.fill("#be");
  await page.getByRole("listbox", { name: "Channels" }).waitFor();
  await composer.fill("/re");
  await page.getByRole("listbox", { name: "Skills" }).waitFor();
  assert.ok(
    skillReads.some((path) => path.includes(`/w/${key(1)}/participants/a/`)),
  );
  await composer.fill("@other #beta a private message");
  await composer.press("Escape");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByText("Private reply for a", { exact: true }).waitFor();
  assert.equal(posts.length, 2, "a stale CSRF token retries once");
  assert.equal(posts[0].body.request_id, posts[1].body.request_id);
  for (const post of posts) {
    assert.equal(post.address, "a");
    assert.equal(post.space, key(1));
    assert.deepEqual(Object.keys(post.body).sort(), ["request_id", "source"]);
  }
  const incoming = message("beta-unread", "Private incoming beta", "b");
  conversations.get("b").push(incoming);
  peers[1].thread_id = "private-thread";
  peers[1].last_message_id = incoming.message_id;
  peers[1].last_agent_message_id = incoming.message_id;
  peers[1].last_activity_ms = Date.now();
  await b.getByRole("img", { name: "Unread direct message" }).waitFor();
  await page.getByRole("button", { name: "alpha", exact: true }).click();
  await page.getByText("Public channel 1", { exact: true }).waitFor();
  assert.equal(
    await page.getByText("Private reply for a", { exact: true }).count(),
    0,
  );
  assert.equal(
    await page.getByText("Private incoming beta", { exact: true }).count(),
    0,
  );
  assert.equal(await page.locator(".channels-navigation .unread").count(), 0);
  await page.getByRole("link", { name: "All activity" }).click();
  assert.equal(
    await page.getByText("Private incoming beta", { exact: true }).count(),
    0,
  );
  await b.click();
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      value: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  assert.equal(await composer.inputValue(), "beta draft");
  await page.getByText("Private incoming beta", { exact: true }).waitFor();
  await page.waitForTimeout(1700);
  assert.equal(
    await b.getByRole("img", { name: "Unread direct message" }).count(),
    1,
    "hidden tab keeps unread",
  );
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      value: false,
    });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await page.waitForFunction(
    (key) =>
      !document
        .querySelector(`[data-dm-key="${key}"]`)
        .classList.contains("unread"),
    `${key(2)}/b`,
  );
  await composer.fill("Beta response");
  await composer.press("Enter");
  await page.getByText("Private reply for b", { exact: true }).waitFor();
  assert.equal(posts.at(-1).address, "b");
  delayBeta = true;
  await a.click();
  await b.click();
  await a.click();
  await page.waitForTimeout(1200);
  assert.equal(
    await page.getByText("Private incoming beta", { exact: true }).count(),
    0,
    "late beta history cannot replace alpha",
  );
  await page.getByRole("button", { name: "Peek", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".peek"));
  await page
    .getByRole("button", { name: "Text snapshot", exact: true })
    .click();
  await page.getByText("Pane closed", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Close screen", exact: true }).click();
  assert.ok(
    screenReads.some((path) => path.includes(`/w/${key(1)}/participants/a/`)),
  );
  const at = Date.now();
  const gapMessage = (id) => ({
    ...message(`gap-${id}`, `Gap message ${id}`),
    at_ms: at + id,
  });
  conversations
    .get("a")
    .push(...Array.from({ length: 201 }, (_, index) => gapMessage(index + 1)));
  await page.getByText("Gap message 201", { exact: true }).waitFor();
  assert.equal(
    await page.getByText("Private reply for a", { exact: true }).count(),
    0,
  );
  const loadEarlier = page.getByRole("button", {
    name: "Load earlier messages",
    exact: true,
  });
  delayOlder = true;
  await loadEarlier.click();
  await page.waitForTimeout(100);
  conversations
    .get("a")
    .push(
      ...Array.from({ length: 200 }, (_, index) => gapMessage(index + 202)),
    );
  await page.getByText("Gap message 401", { exact: true }).waitFor();
  await page.waitForTimeout(4200);
  assert.equal(
    await page.getByText("Gap message 2", { exact: true }).count(),
    0,
    "an older page for a replaced cursor cannot create a gap",
  );
  delayOlder = false;
  await loadEarlier.click();
  await page.getByText("Gap message 202", { exact: true }).waitFor();
  assert.equal(
    await page.getByText("Gap message 302", { exact: true }).count(),
    1,
  );
  await mkdir("/tmp/direct-messages-channels", { recursive: true });
  await page.screenshot({
    path: "/tmp/direct-messages-channels/desktop-light.png",
  });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({
    path: "/tmp/direct-messages-channels/desktop-dark.png",
  });
  await page.getByRole("button", { name: /Closed/ }).click();
  await page.locator(`[data-dm-key="${key(3)}/closed"]`).click();
  await page.getByText("Retained closed history", { exact: true }).waitFor();
  const timeline = page.getByRole("region", {
    name: "Direct conversation with @claude · #archived",
  });
  await timeline.focus();
  assert.equal(
    await timeline.evaluate((node) => node === document.activeElement),
    true,
  );
  await timeline.evaluate((node) => {
    node.scrollTop = 0;
  });
  await timeline.press("PageDown");
  await page.waitForFunction(
    () => document.querySelector(".dm-timeline").scrollTop > 100,
  );
  await timeline.evaluate((node) => {
    node.scrollTop = 0;
  });
  await timeline.press("ArrowDown");
  await page.waitForFunction(
    () => document.querySelector(".dm-timeline").scrollTop > 0,
  );
  assert.equal(await composer.isDisabled(), true);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({
    path: "/tmp/direct-messages-channels/mobile-dark.png",
  });
  assert.ok(directoryReads >= 2);
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth > innerWidth,
    ),
    false,
  );
  assert.deepEqual(errors, []);
});
