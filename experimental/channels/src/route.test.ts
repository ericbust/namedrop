import assert from "node:assert/strict";
import { test } from "node:test";
import type { AddressId, Directory } from "./model.ts";
import {
  chatOf,
  chatPeople,
  type RouteLine,
  routeLine,
  sameChat,
} from "./route.ts";
import { threadGroups } from "./store.ts";
import { parseEvent } from "./wire.ts";

const key = "a".repeat(20);
function member(id: string, kind: unknown, activity = "idle") {
  return {
    address_id: id,
    alias: id === "operator" ? "you" : `@${id}`,
    kind,
    activity,
    role: null,
    pane_label: null,
  };
}
const agent = { type: "agent", runtime: "codex" };
const roster = [
  member("claude", agent),
  member("codex", agent),
  member("old", agent, "retired"),
  member("shell", { type: "terminal" }),
  member("operator", { type: "human" }),
];
type Row = Record<string, unknown>;
function post(
  text: string,
  channel: unknown,
  to: string[] = [],
  extra: Row = {},
): Row {
  return {
    message_id: "post",
    thread_id: "post",
    in_reply_to: null,
    at_ms: 1_000,
    author: "human",
    kind: "mention",
    body: { text, truncated: false },
    legs: to.map((id) => ({
      to: id,
      stage: { stage: "working", acked_at_ms: 1 },
    })),
    // The server lists the agents a post names, as it read them when it stored the post.
    channel:
      typeof channel === "object" &&
      channel !== null &&
      !("mentioned" in channel)
        ? { ...channel, mentioned: named(text) }
        : channel,
    ...extra,
  };
}
function named(text: string): string[] {
  return [
    ...new Set(
      Array.from(
        text.matchAll(/(?<![\w\\])@([a-z0-9]+(?:-[a-z0-9]+)*)/g),
        ([, alias]) => alias ?? "",
      ),
    ),
  ];
}
function reply(id: string, author: string, at: number, extra: Row = {}): Row {
  return {
    message_id: id,
    thread_id: "post",
    in_reply_to: "post",
    at_ms: at,
    author,
    kind: "reply",
    body: { text: id, truncated: false },
    legs: [],
    ...extra,
  };
}
function room(...rows: Row[]) {
  const event = parseEvent(
    "space",
    JSON.stringify({
      key,
      frame: {
        wire: 1,
        space: {
          key,
          workspace_id: "w1",
          display_name: "alpha",
          addressable_label: "alpha",
        },
        health: { level: "calm" },
        participants: roster,
        messages: rows,
        threads: [],
        attention: [],
      },
    }),
  );
  assert.ok(event?.type === "space");
  const { channel } = event;
  const groups = threadGroups({
    ...channel,
    runs: [[...channel.messages.keys()]],
  });
  const directory: Directory = { members: channel.members, homes: new Map() };
  return {
    directory,
    line(id = "post", replies = 0): RouteLine | null {
      const message = channel.messages.get(id);
      const thread = message && groups.get(message.threadId);
      assert.ok(message && thread?.room?.kind === "channel");
      return routeLine(message, thread.room, directory, replies);
    },
    chat(id: string, replies = 0) {
      const message = channel.messages.get(id);
      const thread = message && groups.get(message.threadId);
      assert.ok(message && thread);
      return chatOf({
        message,
        thread,
        reference: null,
        directory,
        replies,
      });
    },
  };
}
const ids = (...names: string[]) => names as AddressId[];
const working = { stage: "working", ackedAt: 1 } as const;
const routed = (
  reason: unknown = "build question",
  router: unknown = "claude-haiku",
) => ({
  state: "routed",
  to: ["claude"],
  reason,
  router,
});

test("route line: a post that mentions no agent has no line, and shows its reply count alone", () => {
  for (const channel of [
    { state: "unrouted", to: [] },
    { state: "routed", to: [] },
  ]) {
    const chat = room(post("heads up: rotating keys at 3pm", channel));
    assert.equal(chat.line(), null);
    assert.deepEqual(chat.line("post", 3), {
      icon: null,
      tone: "muted",
      tip: null,
      parts: [{ kind: "replies", count: 3 }],
    });
  }
});
test("route line: routing reads Checking who this is for, with the clock", () => {
  assert.deepEqual(
    room(post("can @claude look?", { state: "routing" })).line("post", 1),
    {
      icon: "clock",
      tone: "muted",
      tip: null,
      parts: [
        { kind: "state", state: "checking" },
        { kind: "replies", count: 1 },
      ],
    },
  );
});
test("route line: routed names who got it, the reason and the receipt, with the router in the tooltip", () => {
  assert.deepEqual(
    room(post("can @claude look?", routed(), ["claude"])).line(),
    {
      icon: "arrow",
      tone: "muted",
      tip: { kind: "router", name: "claude-haiku" },
      parts: [
        { kind: "names", ids: ids("claude") },
        { kind: "reason", text: "build question" },
        { kind: "receipt", id: "claude", named: false, stage: working },
      ],
    },
  );
  assert.deepEqual(
    room(post("can @claude look?", routed(null, null), ["claude"])).line()
      ?.parts,
    [
      { kind: "names", ids: ids("claude") },
      { kind: "receipt", id: "claude", named: false, stage: working },
    ],
    "no reason and no router",
  );
  assert.equal(
    room(post("@claude?", routed(null, null), ["claude"])).line()?.tip,
    null,
  );
});
test("route line: a routed post offers Send anyway for the one mentioned agent it did not reach", () => {
  assert.deepEqual(
    room(
      post("thanks @codex, can @claude check the mac build?", routed(), [
        "claude",
      ]),
    ).line()?.parts,
    [
      { kind: "names", ids: ids("claude") },
      { kind: "reason", text: "build question" },
      { kind: "receipt", id: "claude", named: false, stage: working },
      { kind: "mentioned", aliases: ["@codex"] },
      { kind: "send", aliases: ["@codex"], form: "anyway" },
    ],
  );
});
test("route line: an unrouted post that mentions agents says so with the inbox, and several offers read Send to", () => {
  const one = room(
    post("nice catch by @codex", { state: "unrouted", to: [] }),
  ).line();
  assert.deepEqual(one, {
    icon: "inbox",
    tone: "muted",
    tip: null,
    parts: [
      { kind: "mentioned", aliases: ["@codex"] },
      { kind: "send", aliases: ["@codex"], form: "anyway" },
    ],
  });
  assert.deepEqual(
    room(post("credit to @claude and @codex", { state: "unrouted" })).line(
      "post",
      2,
    )?.parts,
    [
      { kind: "mentioned", aliases: ["@claude", "@codex"] },
      { kind: "send", aliases: ["@claude", "@codex"], form: "to" },
      { kind: "replies", count: 2 },
    ],
    "in the order the text names them",
  );
  assert.deepEqual(
    room(post("thanks @codex", routed(), [])).line(),
    one,
    "routed with no one reached reads as unrouted",
  );
});
test("route line: Send anyway stops offering an agent the thread has reached since", () => {
  const unrouted = { state: "unrouted", to: [] };
  const sent = reply("sent", "human", 2_000, {
    legs: [{ to: "codex", stage: { stage: "delivered" } }],
  });
  assert.deepEqual(
    room(post("nice catch by @codex", unrouted), sent).line("post", 1),
    {
      icon: null,
      tone: "muted",
      tip: null,
      parts: [{ kind: "replies", count: 1 }],
    },
    "a follow-up sent to @codex leaves only the reply count",
  );
  assert.deepEqual(
    room(
      post("credit to @claude and @codex", unrouted),
      reply("answer", "codex", 2_000),
    ).line()?.parts,
    [
      { kind: "mentioned", aliases: ["@claude"] },
      { kind: "send", aliases: ["@claude"], form: "anyway" },
    ],
    "an agent who wrote in the thread is not offered",
  );
  assert.deepEqual(
    room(
      post("can @claude or @codex look?", {
        state: "router_failed",
        to: [],
      }),
      sent,
    ).line()?.parts,
    [
      { kind: "state", state: "failed" },
      { kind: "send", aliases: ["@claude"], form: "to" },
    ],
  );
});
test("route line: router_failed is in the failed tone, always offers Send to, and explains itself", () => {
  assert.deepEqual(
    room(post("can @codex look?", { state: "router_failed" })).line("post", 1),
    {
      icon: "alert",
      tone: "failed",
      tip: { kind: "unreachable" },
      parts: [
        { kind: "state", state: "failed" },
        { kind: "send", aliases: ["@codex"], form: "to" },
        { kind: "replies", count: 1 },
      ],
    },
  );
  assert.deepEqual(
    room(post("no one named", { state: "router_failed" })).line()?.parts,
    [{ kind: "state", state: "failed" }],
  );
});
test("route line: replies and follow-ups name their recipients, and a reply that woke someone says pulled in", () => {
  const chat = room(
    post("can @claude look?", routed(), ["claude"]),
    reply("woke", "claude", 2_000, {
      body: { text: "@codex is the cache stale?", truncated: false },
      legs: [
        { to: "operator", stage: { stage: "delivered" } },
        { to: "codex", stage: { stage: "delivered" } },
      ],
      woke: ["codex"],
    }),
    reply("plain", "human", 3_000, {
      body: { text: "and @codex too", truncated: false },
      legs: [{ to: "claude", stage: { stage: "queued" } }],
    }),
    reply("to-you", "codex", 4_000, {
      legs: [{ to: "human", stage: { stage: "delivered" } }],
    }),
  );
  assert.deepEqual(chat.line("woke")?.parts, [
    { kind: "names", ids: ids("codex") },
    { kind: "pulled-in" },
  ]);
  assert.deepEqual(chat.line("plain")?.parts, [
    { kind: "names", ids: ids("claude") },
    { kind: "receipt", id: "claude", named: false, stage: { stage: "queued" } },
  ]);
  assert.equal(chat.line("to-you"), null, "a reply to the operator only");
});
test("route line: legs to the operator never show, and several recipients name their receipts", () => {
  const chat = room(
    post("@claude and @codex, look", routed(), [
      "operator",
      "claude",
      "codex",
      "human",
    ]),
  );
  assert.deepEqual(chat.line()?.parts, [
    { kind: "names", ids: ids("claude", "codex") },
    { kind: "reason", text: "build question" },
    { kind: "receipt", id: "claude", named: true, stage: working },
    { kind: "receipt", id: "codex", named: true, stage: working },
  ]);
});
test("route line: receipts hide once every recipient wrote at or after the message, not after an older reply", () => {
  const follow = (claudeAt: number, codexAt: number) =>
    room(
      post("can @claude look?", routed(), ["claude"]),
      reply("early", "claude", 1_500),
      reply("ask", "human", 2_000, {
        legs: [
          { to: "claude", stage: { stage: "working", acked_at_ms: 1 } },
          { to: "codex", stage: { stage: "delivered" } },
        ],
      }),
      reply("claude-later", "claude", claudeAt),
      reply("codex-later", "codex", codexAt),
    );
  const receipts = (claudeAt: number, codexAt: number, id: string) =>
    follow(claudeAt, codexAt)
      .line(id)
      ?.parts.filter((part) => part.kind === "receipt").length;
  assert.equal(receipts(1_600, 1_700, "post"), 0, "the post was answered");
  assert.equal(
    receipts(1_600, 1_700, "ask"),
    2,
    "older replies do not answer a follow-up",
  );
  assert.equal(receipts(2_500, 1_700, "ask"), 2, "one of two answered");
  assert.equal(receipts(2_500, 2_000, "ask"), 0, "both wrote at or after it");
});
test("chat: only rooms carry one, an agent conversation has no route line, and comparisons go by value", () => {
  const request = room(post("@claude look", null, ["claude"]));
  assert.equal(request.chat("post"), null);
  const agents = room(
    { ...post("@codex check", null, ["codex"]), author: "claude" },
    reply("answer", "codex", 2_000),
  );
  assert.deepEqual(agents.chat("answer"), {
    kind: "agents",
    reference: null,
    route: null,
  });
  assert.deepEqual(
    agents.chat("post", 1),
    {
      kind: "agents",
      reference: null,
      route: {
        icon: null,
        tone: "muted",
        tip: null,
        parts: [{ kind: "replies", count: 1 }],
      },
    },
    "an agent conversation's first message links its replies like a channel post",
  );
  const channel = room(post("can @claude look?", routed(), ["claude"]));
  const a = channel.chat("post");
  const b = channel.chat("post");
  assert.notEqual(a, b);
  assert.equal(sameChat(a, b), true);
  assert.equal(
    sameChat(
      a,
      room(post("can @claude look?", routed("other"), ["claude"])).chat("post"),
    ),
    false,
  );
  assert.equal(sameChat(a, null), false);
  assert.deepEqual(chatPeople(a), ["claude", "claude"]);
  assert.deepEqual(chatPeople(null), []);
});
test("route line: Send anyway offers the agents the server says the post names, not words that look like mentions", () => {
  const unrouted = (mentioned: string[]) => ({
    state: "unrouted",
    to: [],
    mentioned,
  });
  assert.equal(
    room(post("thanks @codex", unrouted([]))).line(),
    null,
    "the server found no agent mention, so there is no offer",
  );
  assert.deepEqual(
    room(post("thanks to our reviewer", unrouted(["claude"]))).line()?.parts,
    [
      { kind: "mentioned", aliases: ["@claude"] },
      { kind: "send", aliases: ["@claude"], form: "anyway" },
    ],
  );
});
