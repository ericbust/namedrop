import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { after, before, test } from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import type { Draft, DraftKey } from "./model.ts";
import { exitFor, initialState, reduce, runsByMessage } from "./store.ts";
import { parseEvent } from "./wire.ts";

const onControl = async () => ({ kind: "done" }) as const;

let vite: ViteDevServer;
before(async () => {
  vite = await createServer({
    server: { middlewareMode: true, hmr: false, ws: false },
  });
});
after(async () => {
  await vite.close();
});
function load<T>(file: string): Promise<T> {
  return vite.ssrLoadModule(file) as Promise<T>;
}
async function fixtures() {
  return (
    await readFile(
      new URL("./fixtures/real-frames.jsonl", import.meta.url),
      "utf8",
    )
  )
    .trim()
    .split("\n");
}
function text(markup: string): string {
  return markup.replace(/<[^>]*>/g, "");
}
function buttons(markup: string): string[] {
  return [...markup.matchAll(/<button[^>]*>(.*?)<\/button>/gs)].map(
    ([, inner]) => text(inner ?? "").replaceAll("&#x27;", "'"),
  );
}
function space(value: unknown) {
  const event = parseEvent("space", JSON.stringify(value));
  assert.ok(event?.type === "space");
  return event;
}

test("item 1: rendered display names never invent mentions and exact tokens stay separate", async () => {
  const { Sidebar } = await load<typeof import("./ui/Sidebar.tsx")>(
    "/src/ui/Sidebar.tsx",
  );
  const { ChannelHeader } = await load<typeof import("./ui/ChannelHeader.tsx")>(
    "/src/ui/ChannelHeader.tsx",
  );
  const lines = await fixtures();
  for (const line of lines) assert.ok(parseEvent("space", line));
  const fixture = JSON.parse(lines[2] ?? "null");
  for (const [name, token] of [
    ["all", "actual-token"],
    ["My Project", null],
    ["testbed link-channels-nd1", "testbed-link-channels-nd1"],
  ] satisfies [string, string | null][]) {
    fixture.frame.space.display_name = name;
    fixture.frame.space.addressable_label = token;
    const event = space(fixture);
    const state = reduce(initialState(), event);
    const header = renderToStaticMarkup(
      createElement(ChannelHeader, {
        toggle: null,
        channel: event.channel,
        suffix: null,
      }),
    );
    assert.equal(text(header.match(/<h1[^>]*>(.*?)<\/h1>/)?.[1] ?? ""), name);
    assert.ok(
      header.includes(token === null ? ">No mention label<" : `>#${token}<`),
      header,
    );
    const sidebar = renderToStaticMarkup(
      createElement(Sidebar, {
        state,
        busy: 0,
        id: "sidebar",
        drawer: null,
        toggleFocusLost: { current: false },
        onSelect() {},
        onActivity() {},
      }),
    );
    assert.equal(
      text(sidebar.match(/<button[^>]*>(.*?)<\/button>/)?.[1] ?? ""),
      name,
    );
    assert.equal(sidebar.includes("#"), false, "the sidebar row has no token");
  }
});

test("a channel whose post was refused while the reader was elsewhere shows the alert icon in its sidebar row", async () => {
  const { Sidebar } = await load<typeof import("./ui/Sidebar.tsx")>(
    "/src/ui/Sidebar.tsx",
  );
  const lines = await fixtures();
  const event = space(JSON.parse(lines[0] ?? "null"));
  const composer = { key: event.channel.key, thread: null };
  const sending = reduce(
    reduce(reduce(initialState(), event), {
      type: "edit",
      composer,
      text: "@ghost hi",
    }),
    {
      type: "send",
      outgoing: {
        requestId: "r1",
        composer,
        source: "@ghost hi",
        inReplyTo: null,
        at: 1,
      },
    },
  );
  const failed = reduce(sending, {
    type: "send-failed",
    requestId: "r1",
    status: 400,
    message: "Unknown mention: @ghost.",
  });
  const marker = /role="img" aria-label="A post wasn(?:'|&#x27;)t sent"/g;
  const render = (state: typeof failed) =>
    renderToStaticMarkup(
      createElement(Sidebar, {
        state,
        busy: 0,
        id: "sidebar",
        drawer: null,
        toggleFocusLost: { current: false },
        onSelect() {},
        onActivity() {},
      }),
    );
  assert.equal((render(sending).match(marker) ?? []).length, 0);
  assert.equal((render(failed).match(marker) ?? []).length, 1);
  const edited = reduce(failed, {
    type: "edit",
    composer,
    text: "@codex hi",
  });
  assert.equal((render(edited).match(marker) ?? []).length, 0);
});

test("item 6: paused and inactive channels with attention have no Needs you shortcuts, dots or banner", async () => {
  const { Sidebar } = await load<typeof import("./ui/Sidebar.tsx")>(
    "/src/ui/Sidebar.tsx",
  );
  const { ChannelHeader } = await load<typeof import("./ui/ChannelHeader.tsx")>(
    "/src/ui/ChannelHeader.tsx",
  );
  const { NeedsYou } = await load<typeof import("./ui/NeedsYou.tsx")>(
    "/src/ui/NeedsYou.tsx",
  );
  const fixture = JSON.parse((await fixtures())[0] ?? "null");
  fixture.frame.attention = [
    {
      source_kind: "coordination",
      summary: { text: "Approve this", truncated: false },
      opened_at_ms: 1,
    },
  ];
  const health = {
    paused: "Paused · deliveries held",
    inactive: "Not active",
    future_health: null,
    needs_you: null,
  };
  for (const [level, summary] of Object.entries(health)) {
    fixture.frame.health =
      level === "needs_you" ? { level, items: 1 } : { level };
    const event = space(fixture);
    const state = reduce(initialState(), event);
    const sidebar = renderToStaticMarkup(
      createElement(Sidebar, {
        state,
        busy: 0,
        id: "sidebar",
        drawer: null,
        toggleFocusLost: { current: false },
        onSelect() {},
        onActivity() {},
      }),
    );
    const header = renderToStaticMarkup(
      createElement(ChannelHeader, {
        toggle: null,
        channel: event.channel,
        suffix: null,
      }),
    );
    const banner = renderToStaticMarkup(
      createElement(NeedsYou, {
        channel: event.channel,
        directory: { members: event.channel.members, homes: new Map() },
        onFocus() {},
        onControl,
      }),
    );
    const needsYou = level === "needs_you";
    assert.equal(sidebar.includes('aria-label="Needs you" class='), needsYou);
    assert.equal(
      sidebar.includes('role="img" aria-label="Needs you"'),
      needsYou,
    );
    assert.equal(sidebar.includes("Coordination needs a decision"), needsYou);
    assert.equal(banner.includes('aria-label="Attention needed"'), needsYou);
    assert.equal(banner.includes("Coordination needs a decision"), needsYou);
    assert.equal(banner.includes("Approve this"), false, "details stay closed");
    const shown = header.match(/class="health"[^>]*>(.*?)<\/span>/)?.[1];
    assert.equal(shown === undefined ? null : text(shown), summary);
    assert.equal(header.includes('data-tone="held"'), level === "paused");
  }
});

test("fix5: future member values show neutral labels and keep role and pane in the tooltip", async () => {
  const { MemberStrip } = await load<typeof import("./ui/MemberStrip.tsx")>(
    "/src/ui/MemberStrip.tsx",
  );
  const fixture = JSON.parse((await fixtures())[0] ?? "null");
  const member = fixture.frame.participants[0];
  fixture.frame.participants = [
    {
      ...member,
      address_id: "future-member",
      alias: "Future",
      activity: "future_activity",
      kind: { type: "future_kind" },
      role: null,
      pane_label: null,
    },
    {
      ...member,
      address_id: "role-member",
      alias: "Role",
      activity: "future_activity",
      kind: { type: "future_kind" },
      role: { text: "Builder", truncated: true },
      pane_label: { text: "pane 2", truncated: true },
    },
  ];
  const event = space(fixture);
  const markup = renderToStaticMarkup(
    createElement(MemberStrip, { members: event.channel.members }),
  );
  assert.equal((markup.match(/>Unknown</g) ?? []).length, 2);
  assert.ok(markup.includes('title="Member"'));
  assert.ok(markup.includes('title="Member · Builder… · pane 2…"'));
  assert.equal(
    (markup.match(/class="presence" data-activity="unknown"/g) ?? []).length,
    2,
  );
});

test("design-b: member chips order by state, end with the operator as You without presence, and name the runtime", async () => {
  const { MemberStrip } = await load<typeof import("./ui/MemberStrip.tsx")>(
    "/src/ui/MemberStrip.tsx",
  );
  const fixture = JSON.parse((await fixtures())[3] ?? "null");
  const base = fixture.frame.participants[1];
  const agent = (
    id: string,
    alias: string,
    activity: string,
    kind: unknown = { type: "agent", runtime: "codex" },
  ) => ({ ...base, address_id: id, alias, activity, kind });
  fixture.frame.participants = [
    { ...fixture.frame.participants[0], activity: "working" },
    agent("a", "@idle", "idle", { type: "agent", runtime: "claude-code" }),
    agent("b", "@off", "offline", { type: "agent", runtime: null }),
    agent("c", "@busy", "working", { type: "agent", runtime: "gemini" }),
    agent("d", "@gone", "retired"),
    agent("e", "@ask", "blocked"),
    agent("f", "@wait", "waiting"),
    agent("g", "@shell", "idle", { type: "terminal" }),
  ];
  const event = space(fixture);
  const markup = renderToStaticMarkup(
    createElement(MemberStrip, { members: event.channel.members }),
  );
  const chips = [
    ...markup.matchAll(/<li data-address-id="([^"]+)">(.*?)<\/li>/g),
  ];
  assert.deepEqual(
    chips.map(([, id]) => id),
    ["e", "c", "f", "a", "g", "b", "human"],
  );
  const chip = (id: string) =>
    chips.find(([, chipId]) => chipId === id)?.[2] ?? "";
  assert.equal(text(chip("human")), "YYou");
  assert.equal(chip("human").includes("presence"), false);
  assert.equal(chip("human").includes("Waiting"), false);
  assert.equal(text(chip("e")), "A@askNeeds you");
  assert.ok(chip("e").includes("member is-blocked"));
  assert.ok(chip("a").includes('title="Claude Code"'));
  assert.ok(chip("c").includes('title="Gemini"'));
  assert.ok(chip("b").includes('title="Agent"'));
  assert.ok(chip("f").includes('title="Codex"'));
  assert.ok(chip("g").includes('title="Terminal"'));
  assert.ok(chip("g").includes("avatar sm id-terminal"));
  assert.equal(chip("d"), "");
  assert.equal(markup.includes("claude-code"), false);
});

test("a recently closed member reads Pane closed, muted without a dot, last among agents before You, with a Peek button", async () => {
  const { MemberStrip } = await load<typeof import("./ui/MemberStrip.tsx")>(
    "/src/ui/MemberStrip.tsx",
  );
  const { PeekContext } = await load<typeof import("./ui/PeekContext.tsx")>(
    "/src/ui/PeekContext.tsx",
  );
  const fixture = JSON.parse((await fixtures())[3] ?? "null");
  const agent = fixture.frame.participants[1];
  fixture.frame.participants = [
    {
      ...agent,
      address_id: "closed",
      alias: "@closed",
      activity: "retired",
      recently_closed: true,
    },
    { ...agent, address_id: "off", alias: "@off", activity: "offline" },
    { ...agent, address_id: "odd", alias: "@odd", activity: "mystery" },
    { ...fixture.frame.participants[0], activity: "working" },
  ];
  const event = space(fixture);
  const markup = renderToStaticMarkup(
    createElement(
      PeekContext.Provider,
      { value: { scope: event.channel.key, open() {} } },
      createElement(MemberStrip, { members: event.channel.members }),
    ),
  );
  assert.deepEqual(
    [...markup.matchAll(/<li data-address-id="([^"]+)">/g)].map(([, id]) => id),
    ["off", "odd", "closed", "human"],
  );
  const chip = markup.match(/<li data-address-id="closed">(.*?)<\/li>/)?.[1];
  assert.ok(chip, markup);
  assert.equal(text(chip), "C@closedPane closed");
  assert.equal(chip.includes("presence"), false, chip);
  assert.ok(chip.includes('class="member is-retired"'), chip);
  assert.ok(chip.startsWith('<button type="button"'));
  assert.ok(chip.includes(`data-peek="${event.channel.key}/closed"`));
  assert.ok(chip.endsWith("</button>"));
});

test("an old retired member leaves the strip but remains the author of its history", async () => {
  const { MemberStrip } = await load<typeof import("./ui/MemberStrip.tsx")>(
    "/src/ui/MemberStrip.tsx",
  );
  const { Message } = await load<typeof import("./ui/Message.tsx")>(
    "/src/ui/Message.tsx",
  );
  const fixture = JSON.parse((await fixtures())[3] ?? "null");
  fixture.frame.participants = [
    {
      ...fixture.frame.participants[1],
      address_id: "closed",
      alias: "@closed",
      activity: "retired",
      recently_closed: false,
    },
  ];
  fixture.frame.messages = [
    {
      ...fixture.frame.messages[0],
      author: "closed",
    },
  ];
  const { channel } = space(fixture);
  const strip = renderToStaticMarkup(
    createElement(MemberStrip, { members: channel.members }),
  );
  assert.equal(strip.includes('data-address-id="closed"'), false);
  assert.equal(channel.members.size, 1);
  const message = [...channel.messages.values()][0];
  assert.ok(message);
  const history = renderToStaticMarkup(
    createElement(Message, {
      message,
      directory: { members: channel.members, homes: new Map() },
      recipients: [],
      exit: undefined,
    }),
  );
  assert.ok(history.includes("@closed"), history);
});

test("an escaped mention reads as the plain name in prose, and stays literal in code", async () => {
  const { Message } = await load<typeof import("./ui/Message.tsx")>(
    "/src/ui/Message.tsx",
  );
  const fixture = JSON.parse((await fixtures())[3] ?? "null");
  const text = "\\@codex says 6 times 7 is 42.\n```\nwrite \\@codex here\n```";
  fixture.frame.messages = [
    { ...fixture.frame.messages[0], body: { text, truncated: false } },
  ];
  const { channel } = space(fixture);
  const message = [...channel.messages.values()][0];
  assert.ok(message);
  const markup = renderToStaticMarkup(
    createElement(Message, {
      message,
      directory: { members: channel.members, homes: new Map() },
      recipients: [],
      exit: undefined,
    }),
  );
  assert.ok(markup.includes(">@codex says 6 times 7 is 42."), markup);
  assert.ok(markup.includes("write \\@codex here"), markup);
});

test("a nudge shows tracked work in plain words and keeps the server's text in the tooltip", async () => {
  const { Message } = await load<typeof import("./ui/Message.tsx")>(
    "/src/ui/Message.tsx",
  );
  const fixture = JSON.parse((await fixtures())[3] ?? "null");
  const raw =
    "Coordination coordination-1793308769836090626-2129769-1 (revision 3) remains open.\nAssignment assignment-1793308772078455575-2130363-4 revision 4: needs_continuation.";
  const row = (kind: string, text: string) => {
    fixture.frame.messages = [
      {
        ...fixture.frame.messages[0],
        kind,
        body: { text, truncated: false },
      },
    ];
    const { channel } = space(fixture);
    const message = [...channel.messages.values()][0];
    assert.ok(message);
    return renderToStaticMarkup(
      createElement(Message, {
        message,
        directory: { members: channel.members, homes: new Map() },
        recipients: [],
        exit: undefined,
      }),
    );
  };
  const nudge = row("nudge", raw);
  const body = nudge.match(
    /<span class="body"( title="[^"]*")?>(.*?)<\/span>/s,
  );
  assert.ok(body, nudge);
  assert.equal(
    text(body[2] ?? ""),
    "This coordination (revision 3) remains open.\nAssignment 1, revision 4: needs continuation.",
  );
  assert.equal(body[1], ` title="${raw}"`);
  assert.doesNotMatch(row("nudge", "Please continue"), /title=/);
  assert.ok(row("hook", raw).includes(raw), "a hook keeps its text");
});

test("a terminal run for the request decides Exit over the code in the result body", async () => {
  const { Message } = await load<typeof import("./ui/Message.tsx")>(
    "/src/ui/Message.tsx",
  );
  const fixture = JSON.parse((await fixtures())[0] ?? "null");
  fixture.frame.participants[0] = {
    ...fixture.frame.participants[0],
    address_id: "shell",
    alias: "@shell",
    kind: { type: "terminal" },
  };
  const row = (id: string, extra: Record<string, unknown>) => ({
    message_id: id,
    thread_id: "t",
    in_reply_to: null,
    at_ms: 1,
    author: "human",
    kind: "mention",
    body: { text: id, truncated: false },
    legs: [],
    ...extra,
  });
  fixture.frame.messages = [
    row("request", { legs: [{ to: "shell", stage: { stage: "queued" } }] }),
    row("result", {
      in_reply_to: "request",
      kind: "reply",
      author: "shell",
      body: {
        text: "@shell command finished with exit code 0.\nstdout:\nok\n\nstderr:",
        truncated: false,
      },
    }),
  ];
  const exitShown = (runFor: string) => {
    fixture.frame.terminal_runs = [
      {
        terminal: "shell",
        command: { text: "false", truncated: false },
        started_at_ms: 1,
        exit_code: 1,
        message_id: runFor,
      },
    ];
    const { channel } = space(fixture);
    const result = channel.messages.get("result");
    assert.ok(result);
    const markup = renderToStaticMarkup(
      createElement(Message, {
        message: result,
        directory: { members: channel.members, homes: new Map() },
        recipients: [],
        exit: exitFor(runsByMessage(channel.terminalRuns), result),
      }),
    );
    return text(
      markup.match(/<span class="exit"[^>]*>(.*?)<\/span>/)?.[1] ?? "",
    );
  };
  assert.equal(exitShown("request"), "Exit 1");
  assert.equal(exitShown("another-request"), "Exit 0");
});

test("attention actions show historical evidence and safe responses for every source without loading the timeline", async () => {
  const { AttentionActions } = await load<
    typeof import("./ui/AttentionActions.tsx")
  >("/src/ui/AttentionActions.tsx");
  const { PeekContext } = await load<typeof import("./ui/PeekContext.tsx")>(
    "/src/ui/PeekContext.tsx",
  );
  const value = JSON.parse((await fixtures())[0] ?? "null");
  const responsible = value.frame.participants[0].address_id;
  for (const source of [
    "delivery_attempt",
    "agent_disposition",
    "automatic_reply_limit",
    "coordination",
    "agent_status",
    "context_grant",
    "pane_binding",
    "integration_registration",
  ]) {
    const paneOnly = [
      "agent_status",
      "context_grant",
      "pane_binding",
      "integration_registration",
    ].includes(source);
    value.frame.attention = [
      {
        attention_id: source,
        source_kind: source,
        summary: { text: "Needs you", truncated: false },
        opened_at_ms: 1,
        conversation: {
          thread_id: "historical-thread",
          message_id: "historical-message",
        },
        response: paneOnly
          ? { kind: "pane", address_id: responsible }
          : source === "coordination"
            ? { kind: "message", address_id: responsible }
            : {
                kind: "reply",
                thread_id: "historical-thread",
                message_id: "historical-message",
                address_id: responsible,
              },
      },
    ];
    const { channel } = space(value);
    const item = channel.attention[0];
    assert.ok(item);
    const markup = renderToStaticMarkup(
      createElement(
        PeekContext,
        { value: { scope: channel.key, open() {} } },
        createElement(AttentionActions, {
          item,
          channel,
          absent: null,
          onFocus() {},
          onControl,
        }),
      ),
    );
    assert.deepEqual(
      buttons(markup),
      [
        "Show message",
        paneOnly
          ? "See @codex's screen"
          : source === "coordination"
            ? "Message @codex"
            : "Reply to @codex",
      ],
      source,
    );
    assert.doesNotMatch(markup, /Respond|Peek|in Herdr/, source);
    assert.equal(channel.messages.size, 0);
  }
  value.frame.attention = [
    {
      source_kind: "integration_registration",
      summary: { text: "Repair", truncated: false },
      opened_at_ms: 1,
      response: { kind: "local" },
    },
  ];
  const { channel } = space(value);
  const item = channel.attention[0];
  assert.ok(item);
  const local = renderToStaticMarkup(
    createElement(AttentionActions, {
      item,
      channel,
      absent: null,
      onFocus() {},
      onControl,
    }),
  );
  assert.deepEqual(buttons(local), []);
  assert.doesNotMatch(local, /Namedrop setup|Namedrop Doctor/);
});

test("a Needs-you item keeps Show message, its response and Details on one action row, and its hint says where to answer", async () => {
  const { NeedsYou } = await load<typeof import("./ui/NeedsYou.tsx")>(
    "/src/ui/NeedsYou.tsx",
  );
  const { PeekContext } = await load<typeof import("./ui/PeekContext.tsx")>(
    "/src/ui/PeekContext.tsx",
  );
  const value = JSON.parse((await fixtures())[0] ?? "null");
  const worker = value.frame.participants[0];
  worker.alias = "@worker";
  worker.pane_label = { text: "w1:worker", truncated: false };
  const conversation = { thread_id: "t", message_id: "m" };
  const item = (source: string, response: unknown, extra = {}) => ({
    attention_id: source,
    source_kind: source,
    summary: {
      text: `${"A long server summary ".repeat(6)}end`,
      truncated: false,
    },
    opened_at_ms: 1,
    conversation,
    response,
    ...extra,
  });
  value.frame.health = { level: "needs_you", items: 3 };
  value.frame.attention = [
    item("context_grant", { kind: "pane", address_id: worker.address_id }),
    item(
      "delivery_attempt",
      { kind: "reply", ...conversation, address_id: worker.address_id },
      { subject_alias: "quiet" },
    ),
    item("integration_registration", { kind: "local" }, { conversation: null }),
  ];
  const render = () => {
    const { channel } = space(value);
    const markup = renderToStaticMarkup(
      createElement(
        PeekContext,
        { value: { scope: channel.key, open() {} } },
        createElement(NeedsYou, {
          channel,
          directory: { members: channel.members, homes: new Map() },
          onFocus() {},
          onControl,
        }),
      ),
    );
    return [...markup.matchAll(/<li class="attention-item">(.*?)<\/li>/gs)].map(
      ([, inner]) => inner ?? "",
    );
  };
  const items = render();
  assert.equal(items.length, 3);
  const rows = items.map((inner) => [
    ...inner.matchAll(/<div class="attention-actions">(.*?)<\/div>/gs),
  ]);
  assert.deepEqual(
    rows.map((found) => found.map(([, row]) => buttons(row ?? ""))),
    [
      [["Show message", "See @worker's screen", "Details"]],
      [["Show message", "Reply to @worker", "Details"]],
      [["Details"]],
    ],
  );
  const hint = (inner: string) =>
    inner.match(/<p class="attention-hint"( title="[^"]*")?>(.*?)<\/p>/s);
  assert.deepEqual(hint(items[0] ?? "")?.slice(1), [
    undefined,
    "Namedrop couldn&#x27;t capture it. Ask the sender to share it again.",
  ]);
  assert.deepEqual(hint(items[1] ?? "")?.slice(1), [
    undefined,
    "It ended its turn without replying.",
  ]);
  assert.deepEqual(hint(items[2] ?? "")?.slice(1), [
    undefined,
    "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration.",
  ]);
  value.frame.attention = [
    item(
      "automatic_reply_limit",
      { kind: "pane", address_id: worker.address_id },
      { thread_id: "t", actions: ["continue", "stop"] },
    ),
    item("pane_binding", { kind: "pane", address_id: worker.address_id }),
  ];
  const [limit, binding] = render();
  assert.deepEqual(
    hint(limit ?? "")?.slice(1),
    [undefined, "The agents paused after 10 replies in a row."],
    "beside Continue and Stop the paused hint stands, with no pane tooltip",
  );
  assert.deepEqual(hint(binding ?? "")?.slice(1), [
    ' title="w1:worker"',
    "Answer it in @worker&#x27;s pane in Herdr.",
  ]);
});

test("Activity reuses the attention evidence and response controls", async () => {
  const { ActivityView } = await load<typeof import("./ui/ActivityView.tsx")>(
    "/src/ui/ActivityView.tsx",
  );
  const { activityOf } = await import("./activity.ts");
  const { homesOf } = await import("./store.ts");
  const value = JSON.parse((await fixtures())[0] ?? "null");
  value.frame.participants[0].activity = "idle";
  value.frame.attention = [
    {
      attention_id: "decision",
      source_kind: "coordination",
      subject_alias: null,
      summary: { text: "Decision", truncated: false },
      opened_at_ms: 1,
      conversation: { thread_id: "old", message_id: "report" },
      response: {
        kind: "message",
        address_id: value.frame.participants[0].address_id,
      },
    },
  ];
  const state = reduce(initialState(), space(value));
  const markup = renderToStaticMarkup(
    createElement(ActivityView, {
      toggle: null,
      list: activityOf(state.channels),
      homes: homesOf(state.channels.values(), undefined),
      channels: state.channels,
      open: null,
      onFocus() {},
      onControl,
      onJump() {},
    }),
  );
  assert.ok(markup.includes("Show message"), markup);
  assert.ok(markup.includes(">Message @codex<"), markup);
});

test("focused tracked evidence exposes the channel response path without a rejected thread composer", async () => {
  const { ThreadPane } = await load<typeof import("./ui/ThreadPane.tsx")>(
    "/src/ui/ThreadPane.tsx",
  );
  const event = space(JSON.parse((await fixtures())[0] ?? "null"));
  const { channel } = event;
  const directory = { members: channel.members, homes: new Map() };
  const active = reduce(initialState(), event).channels.get(channel.key);
  assert.ok(active);
  const props = {
    channel: active,
    thread: {
      id: "tracked",
      parents: [
        {
          id: "tracked-message",
          threadId: "tracked",
          inReplyTo: null,
          at: 1,
          author: "human",
          kind: "mention",
          body: { text: "Request", truncated: false },
          legs: [],
          routing: null,
          woke: [],
        },
      ],
      replies: [],
      knownCount: 1,
      room: null,
    },
    runs: new Map(),
    directory,
    completion: {
      current: channel.key,
      channels: [],
      membersOf: () => [...channel.members.values()],
    },
    drafts: new Map(),
    outbox: new Map(),
    posted: new Set<string>(),
    onEdit() {},
    onSend() {},
    onClose() {},
    paneId: "tracked-pane",
    toggle: null,
    inert: false,
  } satisfies Parameters<typeof ThreadPane>[0];
  const ordinary = renderToStaticMarkup(createElement(ThreadPane, props));
  assert.ok(ordinary.includes("textarea"));
  const answer = (response: Parameters<typeof ThreadPane>[0]["response"]) => {
    const markup = renderToStaticMarkup(
      createElement(ThreadPane, { ...props, response }),
    );
    assert.equal(markup.includes("textarea"), false);
    const line = markup.match(
      /<p class="thread-answer" role="status"( title="[^"]*")?>(.*?)<\/p>/s,
    );
    assert.ok(line, markup);
    return {
      text: text(line[2] ?? "").replaceAll("&#x27;", "'"),
      title: line[1],
      buttons: buttons(line[2] ?? ""),
    };
  };
  assert.deepEqual(
    answer({ kind: "message", name: "@codex", draft: "none", prepare() {} }),
    {
      text: "Answer tracked work in the channel. Message @codex",
      title: undefined,
      buttons: ["Message @codex"],
    },
  );
  assert.deepEqual(
    answer({
      kind: "message",
      name: "@codex",
      draft: "prepared",
      prepare() {},
    }),
    {
      text: "Answer tracked work in the channel. Your message to @codex is ready there.",
      title: undefined,
      buttons: [],
    },
  );
  assert.deepEqual(
    answer({ kind: "message", name: "@codex", draft: "kept", prepare() {} }),
    {
      text: "Answer tracked work in the channel.",
      title: undefined,
      buttons: [],
    },
    "the channel's kept-draft notice names the card's button as the retry",
  );
  assert.deepEqual(
    answer({ kind: "pane", name: "@terminal", pane: "w1:term" }),
    {
      text: "Answer it in @terminal's pane in Herdr.",
      title: ' title="w1:term"',
      buttons: [],
    },
  );
  const responder = [...channel.members.values()][0];
  assert.ok(responder);
  assert.equal(responder.alias, "@codex");
  const reply = {
    kind: "reply",
    intent: {
      kind: "reply",
      threadId: "tracked",
      messageId: "tracked-message",
      addressId: responder.id,
      subject: null,
    },
  } as const;
  const unavailable = (
    members: typeof channel.members,
    subject: "@codex" | null,
  ) => {
    const markup = renderToStaticMarkup(
      createElement(ThreadPane, {
        ...props,
        directory: { ...directory, members },
        response: { ...reply, intent: { ...reply.intent, subject } },
      }),
    );
    assert.equal(markup.includes("textarea"), false);
    const line = markup.match(
      /<p class="thread-answer" role="status">(.*?)<\/p>/s,
    );
    assert.ok(line, markup);
    return (line[1] ?? "").replaceAll("&#x27;", "'");
  };
  const closed = new Map(channel.members);
  closed.set(responder.id, { ...responder, activity: "retired" });
  assert.deepEqual(
    [
      unavailable(closed, null),
      unavailable(new Map(), "@codex"),
      unavailable(new Map(), null),
    ],
    [
      "@codex's pane is closed, so it can't take a message.",
      "@codex isn't in this channel anymore.",
      "That agent isn't in this channel anymore.",
    ],
  );
  const kept = renderToStaticMarkup(
    createElement(ThreadPane, {
      ...props,
      drafts: new Map<DraftKey, Draft>([
        [
          "thread:tracked",
          {
            key: channel.key,
            thread: "tracked",
            text: "my words",
            attempt: null,
            failure: null,
          },
        ],
      ]),
      response: reply,
    }),
  );
  assert.ok(kept.includes("textarea"), kept);
  assert.ok(
    kept.includes(
      '<div class="composer-status"><p role="status">Your existing draft was kept. Send or clear it before preparing this reply.</p></div>',
    ),
    kept,
  );
  assert.equal(kept.includes("thread-answer"), false);
});
