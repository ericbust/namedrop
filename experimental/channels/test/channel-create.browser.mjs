import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname } from "node:path";
import { test } from "node:test";
import { chromium } from "playwright-core";

const evidence =
  process.env.CHANNEL_CREATE_EVIDENCE ??
  `${process.env.HOME}/.cache/namedrop-evidence/channel-create/screens`;
const key = (n) => n.toString(16).padStart(20, "0");
const json = (res, body, status = 200) => {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
};
const participant = (address_id, alias, kind, activity) => ({
  address_id,
  alias,
  kind,
  activity,
  role: null,
  persona: null,
  pane_label: null,
  mailbox: [],
  waiting_on: [],
  subagents: [],
});
const folders = [
  "/home/person/Projects/namedrop",
  "/home/person/.herdr/worktrees/namedrop/channel-create-page",
  "/home/person/.herdr/worktrees/namedrop/recap-named",
  "/home/person/Projects/website",
  "/srv/builds",
];

test("New channel and Add agent forms, with an error each, at desktop light and dark and at 390x844", {
  timeout: 120_000,
}, async (t) => {
  const base = JSON.parse(
    (
      await readFile(
        new URL("../src/fixtures/real-frames.jsonl", import.meta.url),
        "utf8",
      )
    )
      .trim()
      .split("\n")[0],
  );
  const space = (n, name, participants) => {
    const value = structuredClone(base);
    value.key = key(n);
    value.frame.space = {
      ...value.frame.space,
      key: key(n),
      workspace_id: `w${n}`,
      display_name: name,
      addressable_label: name,
      not_addressable_reason: null,
      state: "active",
    };
    value.frame.participants = participants;
    value.frame.health = { level: "calm" };
    return value;
  };
  const frames = [
    space(1, "namedrop", [
      participant(
        "a1",
        "@lead",
        { type: "agent", runtime: "claude-code" },
        "working",
      ),
      participant("a2", "@codex", { type: "agent", runtime: "codex" }, "idle"),
      participant("a3", "@shell", { type: "terminal" }, "idle"),
    ]),
    space(2, "website", [
      participant(
        "b1",
        "@writer",
        { type: "agent", runtime: "claude-code" },
        "idle",
      ),
    ]),
  ];
  const answers = { channels: [], agents: [] };
  const clients = new Set();
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    if (url.pathname === "/api/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-store",
      });
      for (const frame of frames)
        res.write(`event: space\ndata: ${JSON.stringify(frame)}\n\n`);
      res.write(
        `event: ready\ndata: ${JSON.stringify({ keys: frames.map((frame) => frame.key) })}\n\n`,
      );
      clients.add(res);
      req.on("close", () => clients.delete(res));
      return;
    }
    if (url.pathname === "/api/session") return json(res, { csrf_token: "t" });
    if (url.pathname === "/api/direct-messages")
      return json(res, { peers: [] });
    if (/^\/w\/[^/]+\/folders$/.test(url.pathname))
      return json(res, { folders });
    const create = /^\/w\/[^/]+\/(channels|agents)$/.exec(url.pathname)?.[1];
    if (req.method === "POST" && create !== undefined) {
      const [status, body] = answers[create].shift();
      return json(res, body, status);
    }
    if (/^\/w\/[^/]+\/messages$/.test(url.pathname)) return json(res, []);
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
  await mkdir(evidence, { recursive: true });
  const written = [];
  const errors = [];
  async function page(viewport, colorScheme) {
    const opened = await browser.newPage({
      viewport,
      colorScheme,
      locale: "en-US",
    });
    opened.setDefaultTimeout(8000);
    opened.on("pageerror", (error) => errors.push(error.message));
    await opened.goto(`http://127.0.0.1:${server.address().port}/w/${key(1)}`);
    await opened.locator(".connection-dot.live").waitFor({ state: "attached" });
    return opened;
  }
  async function shot(opened, name) {
    assert.equal(
      await opened.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
      true,
      `${name} has no horizontal scroll`,
    );
    const path = `${evidence}/${name}.png`;
    await opened.screenshot({ path, animations: "disabled" });
    written.push(path);
  }
  async function newChannel(opened, phone) {
    if (phone) await opened.locator(".channels-toggle").click();
    await opened
      .getByRole("button", { name: "New channel", exact: true })
      .click();
    const form = opened.getByRole("form", { name: "New channel" });
    await form.getByRole("button", { name: "/srv/builds" }).waitFor();
    await form.getByLabel("Name", { exact: true }).fill("Docs site");
    await form
      .getByRole("button", {
        name: "/home/person/.herdr/worktrees/namedrop/channel-create-page",
      })
      .click();
    return form;
  }
  async function addAgent(opened) {
    await opened
      .locator(".member-row")
      .getByRole("button", { name: "Add agent" })
      .click();
    const form = opened.getByRole("form", { name: "Add agent" });
    await form.getByLabel("Name", { exact: true }).fill("reviewer");
    await form.getByLabel("Persona").fill("Reviews changes before they merge");
    return form;
  }
  const desktop = { width: 1440, height: 900 };
  const phone = { width: 390, height: 844 };
  for (const scheme of ["light", "dark"]) {
    const opened = await page(desktop, scheme);
    await newChannel(opened, false);
    await shot(opened, `new-channel-desktop-${scheme}`);
    await opened.keyboard.press("Escape");
    await addAgent(opened);
    await shot(opened, `add-agent-desktop-${scheme}`);
    await opened.close();
  }
  {
    const opened = await page(phone, "light");
    await newChannel(opened, true);
    await shot(opened, "new-channel-phone");
    await opened.keyboard.press("Escape");
    await opened.keyboard.press("Escape");
    await addAgent(opened);
    await shot(opened, "add-agent-phone");
    await opened.close();
  }
  answers.channels.push([
    400,
    { error: "That folder doesn't exist. Namedrop never creates folders." },
  ]);
  answers.agents.push([
    409,
    {
      error:
        "@reviewer is already in use in this channel. Pick a different name.",
    },
  ]);
  {
    const opened = await page(desktop, "light");
    const form = await newChannel(opened, false);
    await form.getByLabel("Folder", { exact: true }).fill("~/Projects/docs");
    await form.getByRole("button", { name: "Create", exact: true }).click();
    await form.getByRole("alert").waitFor();
    await shot(opened, "new-channel-error-desktop-light");
    await opened.keyboard.press("Escape");
    const agent = await addAgent(opened);
    await agent.getByRole("button", { name: "Add", exact: true }).click();
    await agent.getByRole("alert").waitFor();
    await shot(opened, "add-agent-error-desktop-light");
    await opened.close();
  }
  answers.channels.push([
    503,
    {
      error:
        "Herdr isn't reachable. Check that Herdr is running, then try again.",
    },
  ]);
  {
    const opened = await page(phone, "dark");
    const form = await newChannel(opened, true);
    await form.getByRole("button", { name: "Create", exact: true }).click();
    await form.getByRole("alert").waitFor();
    await shot(opened, "new-channel-error-phone-dark");
    await opened.keyboard.press("Escape");
    await opened.keyboard.press("Escape");
    const agent = await addAgent(opened);
    await agent.getByLabel("Name", { exact: true }).fill("Code Reviewer");
    await agent.getByRole("alert").waitFor();
    await shot(opened, "add-agent-error-phone-dark");
    await opened.close();
  }
  assert.deepEqual(errors, []);
  console.log(written.join("\n"));
});
