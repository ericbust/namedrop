import assert from "node:assert/strict";
import { test } from "node:test";
import type { Alias, Message } from "./model.ts";
import {
  parseDelivered,
  parseEvent,
  parseHistory,
  routeOf,
  routePath,
  sameRoute,
  terminalResult,
} from "./wire.ts";

// CH-1's wire tests, kept with their inputs and read through the UI's domain
// shape. Malformed optional metadata never drops the row, as every other
// optional field in this parser behaves.
const channelRow = {
  message_id: "message-channel",
  thread_id: "thread-channel",
  in_reply_to: null,
  at_ms: 42,
  author: "human",
  kind: "mention",
  body: { text: "Can someone check this?", truncated: false },
  legs: [],
};

test("channel history preserves optional routing and wake metadata", () => {
  const channel = {
    state: "routed",
    to: ["claude"],
    mentioned: ["claude", "codex"],
    reason: "build question",
    router: "claude-haiku",
  };
  const [message] = parseHistory([{ ...channelRow, channel, woke: ["codex"] }]);
  assert.deepEqual(message?.routing, {
    state: "routed",
    mentioned: ["@claude", "@codex"],
    reason: "build question",
    router: "claude-haiku",
  });
  assert.deepEqual(message?.woke, ["@codex"]);
});

test("channel routing tolerates absent sidecar explanations and legacy rows", () => {
  for (const state of ["routing", "routed", "unrouted", "router_failed"]) {
    assert.deepEqual(
      parseHistory([{ ...channelRow, channel: { state, to: [] } }])[0]?.routing,
      state === "routed"
        ? { state, mentioned: [], reason: null, router: null }
        : { state, mentioned: [] },
    );
  }
  const [legacy] = parseHistory([channelRow]);
  assert.equal(legacy?.routing, null);
  assert.deepEqual(legacy?.woke, []);
});

test("channel routing keeps the row when its optional metadata is malformed", () => {
  const read = (channel: unknown) =>
    parseHistory([{ ...channelRow, channel }])[0]?.routing;
  assert.deepEqual(read({ state: "invented", to: [] }), {
    state: "unknown",
    mentioned: [],
  });
  assert.deepEqual(read({ state: "routed", to: [42] }), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: null,
  });
  assert.deepEqual(read({ state: "routed", to: [], reason: 42 }), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: null,
  });
  assert.deepEqual(read({ state: "routed", to: [], router: "" }), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: null,
  });
  assert.deepEqual(read({ state: "unrouted", to: [], mentioned: [42] }), {
    state: "unrouted",
    mentioned: [],
  });
  assert.deepEqual(parseHistory([{ ...channelRow, woke: [42] }])[0]?.woke, []);
});

const result = (text: string, truncated = false) =>
  terminalResult({ text, truncated });
const header = (code: string) =>
  `@shell command finished with exit code ${code}.\nstdout:\n`;

test("terminal results split stdout and stderr and drop the stdout newline", () => {
  const expected = { exitCode: 0, stdout: "hello", stderr: "" };
  assert.deepEqual(result(`${header("0")}hello\n\nstderr:\n`), expected);
  assert.deepEqual(result(`${header("0")}hello\n\nstderr:`), expected);
  assert.deepEqual(result(`${header("0")}a\n\nb\n\nstderr:\n`), {
    exitCode: 0,
    stdout: "a\n\nb",
    stderr: "",
  });
});
test("terminal results keep empty stdout empty", () => {
  const expected = { exitCode: 0, stdout: "", stderr: "" };
  assert.deepEqual(result(`${header("0")}\nstderr:\n`), expected);
  assert.deepEqual(result(`${header("0")}\nstderr:`), expected);
});
test("terminal results keep a non-empty stderr", () => {
  assert.deepEqual(result(`${header("127")}\nstderr:\nsh: nope: not found\n`), {
    exitCode: 127,
    stdout: "",
    stderr: "sh: nope: not found\n",
  });
  assert.deepEqual(result(`${header("2")}partial\n\nstderr:\nboom`), {
    exitCode: 2,
    stdout: "partial",
    stderr: "boom",
  });
});
test("terminal results split at the last stderr marker", () => {
  assert.deepEqual(
    result(`${header("0")}one\nstderr:\ngrep stderr: log\n\nstderr:\nwarn\n`),
    { exitCode: 0, stdout: "one\nstderr:\ngrep stderr: log", stderr: "warn\n" },
  );
  assert.deepEqual(result(`${header("0")}x\nstderr:\n\nstderr:`), {
    exitCode: 0,
    stdout: "x\nstderr:",
    stderr: "",
  });
});
test("terminal results read negative and large exit codes", () => {
  assert.equal(result(`${header("-1")}\nstderr:\n`)?.exitCode, -1);
  assert.equal(
    result(`${header("4294967295")}\nstderr:\n`)?.exitCode,
    4294967295,
  );
  assert.equal(result(`${header("99999999999999999999")}\nstderr:\n`), null);
});
test("truncated terminal results parse only after the stdout marker", () => {
  assert.equal(
    result("@shell command finished with exit code 0.\nstd", true),
    null,
  );
  assert.equal(result("@shell command finished with exit", true), null);
  assert.deepEqual(result(`${header("0")}line 1\nline 2`, true), {
    exitCode: 0,
    stdout: "line 1\nline 2",
    stderr: "",
  });
  assert.equal(result(`${header("0")}line 1\nline 2`), null);
});
test("other bodies are not terminal results", () => {
  assert.equal(result("please reply with the single word pong."), null);
  assert.equal(result("exit code 1"), null);
  assert.equal(result(`Note: ${header("0")}x\n\nstderr:\n`), null);
  assert.equal(
    result("@shell command finished with exit code 0.\nhello\n\nstderr:\n"),
    null,
  );
  assert.equal(
    result("@shell command finished with exit code zero.\nstdout:\n\nstderr:"),
    null,
  );
});

test("routes: one parse of the address and one print, so reload and Back agree", () => {
  const key = "0123456789abcdef0123";
  const at = (pathname: string, search = "") => routeOf({ pathname, search });
  assert.deepEqual(at("/"), { kind: "root" });
  assert.deepEqual(at("/", "?view=activity"), { kind: "activity" });
  assert.deepEqual(at("/", "?n=abc&view=activity"), { kind: "activity" });
  assert.deepEqual(at("/", "?view=other"), { kind: "root" });
  assert.deepEqual(at("/index.html", "?view=activity"), { kind: "root" });
  assert.deepEqual(at(`/w/${key}`), { kind: "channel", key });
  assert.deepEqual(at(`/w/${key}/`, "?view=activity"), {
    kind: "channel",
    key,
  });
  assert.deepEqual(at(`/w/${key.toUpperCase()}`), { kind: "root" });
  assert.deepEqual(at(`/w/${key}/events`), { kind: "root" });
  for (const route of [at("/"), at("/", "?view=activity"), at(`/w/${key}`)]) {
    const [pathname = "", search = ""] = routePath(route).split(/(?=\?)/);
    assert.deepEqual(routeOf({ pathname, search }), route);
  }
  assert.equal(routePath(at("/", "?view=activity")), "/?view=activity");
  assert.equal(sameRoute(at(`/w/${key}`, "?x=1"), at(`/w/${key}`)), true);
  assert.equal(
    sameRoute(at("/", "?view=activity&x=1"), { kind: "activity" }),
    true,
  );
  assert.equal(sameRoute(at("/"), { kind: "activity" }), false);
});

function row(id: string, extra: Record<string, unknown> = {}) {
  return {
    message_id: id,
    thread_id: id,
    in_reply_to: null,
    at_ms: 1,
    author: "human",
    kind: "mention",
    body: { text: id, truncated: false },
    legs: [],
    ...extra,
  };
}
function framed(...rows: unknown[]): ReadonlyMap<string, Message> {
  const key = "a".repeat(20);
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
        participants: [],
        messages: rows,
        threads: [],
        attention: [],
      },
    }),
  );
  assert.ok(
    event?.type === "space",
    "a frame with channel fields still parses",
  );
  return event.channel.messages;
}
const routingOf = (channel: unknown) =>
  framed(row("m", { channel })).get("m")?.routing;

test("channel: absent is an ordinary message, and each state the server sends parses", () => {
  const [plain] = framed(row("m")).values();
  assert.equal(plain?.routing, null);
  assert.deepEqual(plain?.woke, []);
  assert.equal(routingOf(null), null);
  assert.deepEqual(routingOf({ state: "routing", to: [] }), {
    state: "routing",
    mentioned: [],
  });
  assert.deepEqual(
    routingOf({
      state: "routed",
      to: ["claude"],
      reason: "build question",
      router: "claude-haiku",
    }),
    {
      state: "routed",
      mentioned: [],
      reason: "build question",
      router: "claude-haiku",
    },
  );
  assert.deepEqual(routingOf({ state: "unrouted", to: [] }), {
    state: "unrouted",
    mentioned: [],
  });
  assert.deepEqual(routingOf({ state: "router_failed" }), {
    state: "router_failed",
    mentioned: [],
  });
});
test("channel: an unknown, missing or malformed state stays a channel post, and the frame survives", () => {
  for (const channel of [
    { state: "triaged" },
    {},
    { state: 7 },
    "routed",
    [],
    5,
  ])
    assert.deepEqual(
      routingOf(channel),
      { state: "unknown", mentioned: [] },
      JSON.stringify(channel),
    );
  const rows = framed(
    row("odd", { channel: "routed", woke: "codex" }),
    row("next"),
  );
  assert.deepEqual([...rows.keys()], ["odd", "next"]);
});
test("channel: the reason is one line of at most 60 characters, and a blank reason, the no-topic filler or a blank router reads as none", () => {
  const routed = (reason: unknown, router: unknown = "claude-haiku") =>
    routingOf({ state: "routed", reason, router });
  assert.deepEqual(routed("  build\n\tquestion  "), {
    state: "routed",
    mentioned: [],
    reason: "build question",
    router: "claude-haiku",
  });
  const long = routed(`${"a".repeat(58)} ${"b".repeat(20)}`);
  assert.ok(long?.state === "routed");
  assert.equal(long.reason, `${"a".repeat(58)}…`);
  assert.equal(Array.from(long.reason ?? "").length, 59);
  const astral = routed("😀".repeat(70));
  assert.ok(astral?.state === "routed");
  assert.equal(astral.reason, `${"😀".repeat(59)}…`);
  assert.equal(Array.from(astral.reason ?? "").length, 60);
  const exact = routed("x".repeat(60));
  assert.ok(exact?.state === "routed");
  assert.equal(exact.reason, "x".repeat(60));
  assert.deepEqual(routed("   ", "  "), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: null,
  });
  assert.deepEqual(routed("General topic"), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: "claude-haiku",
  });
  assert.deepEqual(routed("General topic question"), {
    state: "routed",
    mentioned: [],
    reason: "General topic question",
    router: "claude-haiku",
  });
  assert.deepEqual(routed(42, { name: "x" }), {
    state: "routed",
    mentioned: [],
    reason: null,
    router: null,
  });
});
test("woke: bare and @ aliases read as member aliases, once each, and anything else is dropped", () => {
  const wokeOf = (woke: unknown) => framed(row("m", { woke })).get("m")?.woke;
  assert.deepEqual(wokeOf(["codex", "@claude", "@@codex", "", 7, null]), [
    "@codex",
    "@claude",
  ]);
  assert.deepEqual(wokeOf("codex"), []);
  assert.deepEqual(wokeOf({ codex: true }), []);
  assert.deepEqual(wokeOf(undefined), []);
});
test("history pages carry the channel state and woke", () => {
  const [page] = parseHistory([
    row("old", {
      channel: { state: "routed", reason: "ci", router: "codex-luna" },
      woke: ["codex"],
    }),
  ]);
  assert.deepEqual(page?.routing, {
    state: "routed",
    mentioned: [],
    reason: "ci",
    router: "codex-luna",
  });
  assert.deepEqual(page?.woke, ["@codex"]);
});

test("deliver answer: an alias the 200 lists was delivered, and any other named alias was not, with its own reason, then the answer's error", () => {
  const read = (body: unknown, to: readonly Alias[] = ["@codex"]) =>
    Object.fromEntries(parseDelivered(JSON.stringify(body), to));
  const done = { kind: "done" };
  const notSent = (message: string) => ({
    kind: "failed",
    status: 200,
    message,
  });
  const reason = "@codex belongs to a different agent now.";
  assert.deepEqual(read({ channel: {}, delivered: ["codex"] }), {
    "@codex": done,
  });
  assert.deepEqual(
    read({ channel: {}, delivered: ["codex"] }, ["@codex", "@claude"]),
    { "@codex": done, "@claude": notSent("") },
  );
  assert.deepEqual(read({ delivered: [] }), { "@codex": notSent("") });
  assert.deepEqual(
    read({
      delivered: [],
      not_delivered: [{ alias: "codex", reason: `  ${reason} ` }],
      error: "Namedrop skipped 1 agent.",
    }),
    { "@codex": notSent(reason) },
  );
  assert.deepEqual(
    read({ delivered: [], error: " Namedrop skipped 1 agent. " }),
    { "@codex": notSent("Namedrop skipped 1 agent.") },
  );
  assert.deepEqual(
    read(
      {
        delivered: ["lead"],
        not_delivered: [{ alias: "claude", reason: "@claude is retired." }],
        error: "Namedrop skipped 2 agents.",
      },
      ["@codex", "@claude", "@lead"],
    ),
    {
      "@codex": notSent("Namedrop skipped 2 agents."),
      "@claude": notSent("@claude is retired."),
      "@lead": done,
    },
  );
});
test("deliver answer: a 200 with no delivered field, or one the page cannot read, delivered every named alias", () => {
  const to: readonly Alias[] = ["@codex", "@claude"];
  const all = { "@codex": { kind: "done" }, "@claude": { kind: "done" } };
  const read = (body: string) => Object.fromEntries(parseDelivered(body, to));
  for (const body of [
    '{"channel":{"state":"unrouted","to":["codex"]}}',
    '{"error":"Namedrop skipped 1 agent.","not_delivered":[{"alias":"codex","reason":"x"}]}',
    "{}",
    "",
    "Sent.",
    "[]",
    "null",
    '"codex"',
    '{"delivered":"codex"}',
    '{"delivered":null}',
    '{"delivered":{"codex":true}}',
    '{"delivered":["codex",7]}',
    '{"delivered":["claude",""]}',
  ])
    assert.deepEqual(read(body), all, body);
});
test("deliver answer: a malformed reason or error reads as no reason", () => {
  const read = (extra: object) =>
    Object.fromEntries(
      parseDelivered(JSON.stringify({ delivered: [], ...extra }), ["@codex"]),
    );
  const noReason = { "@codex": { kind: "failed", status: 200, message: "" } };
  for (const extra of [
    { not_delivered: "codex" },
    { not_delivered: { alias: "codex", reason: "x" } },
    {
      not_delivered: [
        "codex",
        null,
        { alias: 7, reason: "x" },
        { alias: "codex" },
        { alias: "codex", reason: 5 },
        { alias: "codex", reason: "   " },
      ],
    },
    { error: { message: "x" } },
    { error: 404 },
    { error: "  " },
  ])
    assert.deepEqual(read(extra), noReason, JSON.stringify(extra));
});
