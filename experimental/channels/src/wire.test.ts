import assert from "node:assert/strict";
import { test } from "node:test";
import {
  parseHistory,
  routeOf,
  routePath,
  sameRoute,
  terminalResult,
} from "./wire.ts";

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
    reason: "build question",
    router: "claude-haiku",
  };
  const [message] = parseHistory([{ ...channelRow, channel, woke: ["codex"] }]);
  assert.deepEqual(message?.channel, channel);
  assert.deepEqual(message?.woke, ["codex"]);
});

test("channel routing tolerates absent sidecar explanations and legacy rows", () => {
  for (const state of ["routing", "routed", "unrouted", "router_failed"]) {
    const channel = { state, to: [] };
    assert.deepEqual(
      parseHistory([{ ...channelRow, channel }])[0]?.channel,
      channel,
    );
  }
  const [legacy] = parseHistory([channelRow]);
  assert.equal(legacy && "channel" in legacy, false);
  assert.equal(legacy && "woke" in legacy, false);
});

test("channel routing rejects malformed optional metadata", () => {
  for (const channel of [
    { state: "invented", to: [] },
    { state: "routed", to: [42] },
    { state: "routed", to: [], reason: 42 },
    { state: "routed", to: [], router: "" },
  ]) {
    assert.throws(() => parseHistory([{ ...channelRow, channel }]));
  }
  assert.throws(() => parseHistory([{ ...channelRow, woke: [42] }]));
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
