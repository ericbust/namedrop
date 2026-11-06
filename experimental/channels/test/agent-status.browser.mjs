import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer as createViteServer } from "vite";

test("one agent has the same status in channel members, DMs and both Peek headers", {
  timeout: 60000,
}, async (t) => {
  const envelope = JSON.parse(
    (
      await readFile(
        new URL("../src/fixtures/real-frames.jsonl", import.meta.url),
        "utf8",
      )
    )
      .trim()
      .split("\n")[0],
  );
  const { key, frame } = envelope;
  const member = frame.participants[0];
  member.alias = "@claude";
  member.kind = { type: "agent", runtime: "claude-code" };
  member.activity = "blocked";
  frame.messages = [];
  frame.threads = [];
  frame.health = { level: "needs_you", items: 1 };
  const pending = (kind) => [
    {
      source_kind: "agent_status",
      source_id: `pending-${kind}`,
      summary: { text: `@claude needs ${kind}`, truncated: false },
      subject_summary: null,
      opened_at_ms: Date.now(),
      subject_alias: "@claude",
      message_id: null,
      thread_id: null,
      response: { kind: "pane", address_id: member.address_id },
      actions: [],
    },
  ];
  frame.attention = pending("permission");
  const peer = {
    space_key: key,
    address_id: member.address_id,
    alias: "claude",
    workspace_label: "alpha",
    pane_label: null,
    runtime: "claude-code",
    presence: "working",
    closed: false,
    thread_id: null,
    last_activity_ms: null,
    last_message_id: null,
    last_agent_message_id: null,
    attention_count: 0,
  };
  const clients = new Set();
  const publish = () => {
    for (const client of clients)
      client.write(`event: space\ndata: ${JSON.stringify(envelope)}\n\n`);
  };
  const json = (res, value, status = 200) => {
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(JSON.stringify(value));
  };
  const vite = await createViteServer({
    root: fileURLToPath(new URL("../", import.meta.url)),
    server: { middlewareMode: true, hmr: false },
  });
  const server = createServer((req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    if (url.pathname === "/api/events") {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      clients.add(res);
      publish();
      res.write(`event: ready\ndata: ${JSON.stringify({ keys: [key] })}\n\n`);
      req.on("close", () => clients.delete(res));
      return;
    }
    if (url.pathname === "/api/session")
      return json(res, { csrf_token: "test-csrf" });
    if (url.pathname === "/api/direct-messages")
      return json(res, { peers: [peer] });
    if (url.pathname.endsWith("/messages"))
      return json(res, {
        thread_id: null,
        messages: [],
        earlier_omitted: false,
      });
    if (url.pathname.endsWith("/screen"))
      return json(res, { error: "Gone" }, 410);
    if (url.pathname.startsWith("/api/") || url.pathname.endsWith("/reminders"))
      return json(res, { reminders: [] });
    vite.middlewares(req, res);
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
    await vite.close();
  });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 900 },
  });
  page.setDefaultTimeout(8000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/w/${key}`);
  const chip = page.locator(".member").filter({ hasText: "@claude" });
  const dm = page.locator(`[data-dm-key="${key}/${member.address_id}"]`);
  await chip.waitFor();
  await dm.waitFor();
  await mkdir("/tmp/namedrop-agent-status", { recursive: true });
  await page.screenshot({
    path: "/tmp/namedrop-agent-status/status-before.png",
  });
  assert.equal(await chip.locator(".status").textContent(), "Needs you");
  assert.equal(await dm.locator(".row-reason").textContent(), "Needs you");
  await page
    .locator(".attention-title", { hasText: "@claude needs permission" })
    .waitFor();
  await page.screenshot({
    path: "/tmp/namedrop-agent-status/status-needs-you.png",
  });
  await chip.click();
  assert.equal(await page.locator(".peek-presence").textContent(), "Needs you");
  frame.attention = pending("question");
  publish();
  await page
    .locator(".attention-title", { hasText: "@claude needs question" })
    .waitFor();
  assert.equal(await chip.locator(".status").textContent(), "Needs you");
  assert.equal(await dm.locator(".row-reason").textContent(), "Needs you");
  assert.equal(await page.locator(".peek-presence").textContent(), "Needs you");
  await page.getByRole("button", { name: "Close screen", exact: true }).click();
  await dm.click();
  assert.match(await page.locator(".dm-heading").textContent(), /Needs you/);
  await page.getByRole("button", { name: "Peek", exact: true }).click();
  assert.equal(await page.locator(".peek-presence").textContent(), "Needs you");
  member.activity = "idle";
  frame.attention = [];
  frame.health = { level: "calm" };
  publish();
  await page.waitForFunction(
    () => document.querySelector(".peek-presence")?.textContent === "Idle",
  );
  assert.equal(await dm.locator(".row-reason").textContent(), "Idle");
  assert.match(await page.locator(".dm-heading").textContent(), /Idle/);
  await page.getByRole("button", { name: "Close screen", exact: true }).click();
  await page.locator(`.channels-navigation [data-channel-key="${key}"]`).click();
  assert.equal(await chip.locator(".status").textContent(), "Idle");
  assert.equal(await dm.locator(".row-reason").textContent(), "Idle");
  await page.screenshot({
    path: "/tmp/namedrop-agent-status/status-after.png",
  });
  assert.deepEqual(errors, []);
});
