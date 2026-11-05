import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname } from "node:path";
import { test } from "node:test";
import { chromium } from "playwright-core";

const key = "1".repeat(20);
const evidence = process.env.REMINDER_EVIDENCE ?? "/tmp/channels-reminders";
const json = (res, body, status = 200) => {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
};
const message = (id, thread, kind, body, author = "agent") => ({
  message_id: id,
  thread_id: thread,
  in_reply_to:
    id === "public-root" || id === "private-root"
      ? null
      : `${thread === "public-thread" ? "public" : "private"}-root`,
  at_ms: Date.now(),
  author,
  kind,
  body: { text: body, truncated: false },
  legs: [],
});
const reminder = (id) => ({
  reminder_id: id,
  owner: "agent",
  body: { text: "Check the build", truncated: false },
  due_at_ms: Date.now() + 60000,
  repeats_left: 2,
  repeat_until_ms: null,
});

test("quiet reminder rows and scoped Cancel work in thread members and DM header", {
  timeout: 60000,
}, async (t) => {
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
  fixture.key = key;
  fixture.frame.space = {
    ...fixture.frame.space,
    key,
    display_name: "alpha",
    addressable_label: "alpha",
    not_addressable_reason: null,
  };
  fixture.frame.participants = [
    {
      address_id: "agent",
      alias: "@codex",
      kind: { type: "agent", runtime: "codex" },
      activity: "idle",
      role: null,
      pane_label: null,
      mailbox: [],
      waiting_on: [],
      subagents: [],
    },
  ];
  fixture.frame.messages = [
    {
      ...message(
        "public-root",
        "public-thread",
        "mention",
        "Check the public build",
        "human",
      ),
      legs: [{ to: "agent", stage: { stage: "completed", kind: "no_reply" } }],
    },
    message("public-answer", "public-thread", "reply", "I will check later"),
    message(
      "public-reminder",
      "public-thread",
      "reminder",
      "Reminder for @codex: Check the public build",
    ),
  ];
  fixture.frame.threads = [
    {
      thread_id: "public-thread",
      title: { text: "Check the public build", truncated: false },
      message_count: 3,
    },
  ];
  fixture.frame.attention = [];
  fixture.frame.health = { level: "calm" };
  const privateMessages = [
    message(
      "private-root",
      "private-thread",
      "mention",
      "Check privately",
      "human",
    ),
    message(
      "private-reminder",
      "private-thread",
      "reminder",
      "Reminder for @codex: Check privately",
    ),
  ];
  const lists = new Map([
    ["threads/public-thread", [reminder("public-pending")]],
    ["direct-messages/agent", [reminder("private-pending")]],
  ]);
  const cancellations = [];
  let failCancel = true;
  const clients = new Set();
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    if (url.pathname === "/api/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-store",
      });
      res.write(
        `event: space\ndata: ${JSON.stringify(fixture)}\n\nevent: ready\ndata: ${JSON.stringify({ keys: [key] })}\n\n`,
      );
      clients.add(res);
      req.on("close", () => clients.delete(res));
      return;
    }
    if (url.pathname === "/api/session")
      return json(res, { csrf_token: "operator-token" });
    if (url.pathname === "/api/direct-messages")
      return json(res, {
        peers: [
          {
            space_key: key,
            address_id: "agent",
            alias: "codex",
            workspace_label: "alpha",
            pane_label: null,
            runtime: "codex",
            presence: "idle",
            closed: false,
            thread_id: "private-thread",
            last_activity_ms: Date.now(),
            last_message_id: "private-reminder",
            last_agent_message_id: null,
            attention_count: 0,
          },
        ],
      });
    if (url.pathname === `/w/${key}/direct-messages/agent/messages`)
      return json(res, {
        thread_id: "private-thread",
        messages: privateMessages,
        earlier_omitted: false,
      });
    const scoped = new RegExp(
      `^/w/${key}/(threads/public-thread|direct-messages/agent)/reminders(?:/([^/]+)/cancel)?$`,
    ).exec(url.pathname);
    if (scoped) {
      if (req.method === "GET")
        return json(res, { reminders: lists.get(scoped[1]) });
      assert.equal(req.method, "POST");
      assert.equal(req.headers["x-namedrop-csrf"], "operator-token");
      if (failCancel) {
        failCancel = false;
        return json(res, { error: "Try again" }, 409);
      }
      cancellations.push({ scope: scoped[1], id: scoped[2] });
      lists.set(
        scoped[1],
        lists.get(scoped[1]).filter((row) => row.reminder_id !== scoped[2]),
      );
      return json(res, {
        state: "cancelled",
        reminder_id: scoped[2],
        transport_may_have_started: false,
      });
    }
    if (url.pathname.endsWith("/reminders")) return json(res, {}, 404);
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
  await page.goto(`http://127.0.0.1:${server.address().port}/w/${key}`);
  await page.locator('.reply-preview[data-thread-id="public-thread"]').click();
  const thread = page.getByRole("complementary", {
    name: "Thread",
    exact: true,
  });
  const publicRow = thread.locator('[data-message-id="public-reminder"]');
  await publicRow.waitFor();
  assert.match(
    await publicRow.textContent(),
    /⏰.*Reminder for @codex: Check the public build/s,
  );
  assert.equal(
    await publicRow.locator(".receipt,.msg-head,.msg-actions").count(),
    0,
  );
  await thread.locator(".conversation-reminders > summary").click();
  await thread.getByText("2 repeats left", { exact: false }).waitFor();
  const panelBounds = await thread.locator(".reminder-details").boundingBox();
  const threadBounds = await thread.boundingBox();
  assert.ok(panelBounds && threadBounds);
  assert.ok(
    panelBounds.x >= threadBounds.x,
    "reminder panel stays inside the thread pane",
  );
  assert.ok(
    panelBounds.x + panelBounds.width <= threadBounds.x + threadBounds.width,
  );
  await mkdir(evidence, { recursive: true });
  await page.screenshot({ path: `${evidence}/thread-light.png` });
  const cancelPublic = thread.getByRole("button", {
    name: "Cancel reminder Check the build",
    exact: true,
  });
  await cancelPublic.click();
  await thread
    .getByRole("alert")
    .filter({ hasText: "Couldn't cancel" })
    .waitFor();
  await cancelPublic.click();
  await thread.getByText("No pending reminders.").waitFor();
  await page.locator(`[data-dm-key="${key}/agent"]`).click();
  const privateRow = page.locator('[data-message-id="private-reminder"]');
  await privateRow.waitFor();
  assert.match(
    await privateRow.textContent(),
    /Reminder for @codex: Check privately/,
  );
  assert.equal(
    await privateRow.locator(".receipt,.msg-head,.msg-actions").count(),
    0,
  );
  await page.locator(".dm-header .conversation-reminders > summary").click();
  await page
    .getByRole("list", { name: "Pending reminders", exact: true })
    .getByText("Check the build", { exact: true })
    .waitFor();
  await mkdir(evidence, { recursive: true });
  await page.screenshot({ path: `${evidence}/desktop-light.png` });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({ path: `${evidence}/desktop-dark.png` });
  await page.setViewportSize({ width: 1024, height: 700 });
  await page.screenshot({ path: `${evidence}/compact-dark.png` });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth > innerWidth,
    ),
    false,
  );
  await page
    .getByRole("button", {
      name: "Cancel reminder Check the build",
      exact: true,
    })
    .click();
  await page.getByText("No pending reminders.").waitFor();
  assert.deepEqual(cancellations, [
    { scope: "threads/public-thread", id: "public-pending" },
    { scope: "direct-messages/agent", id: "private-pending" },
  ]);
  assert.deepEqual(errors, []);
});
