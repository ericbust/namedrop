import assert from "node:assert/strict";
import test from "node:test";
const { chooseTarget, saveName } = await import("./naming.js").catch(() => ({}));
const operation = saveName ? test : test.skip;
import { normalizeAlias, aliasTaken, formatHcError } from "./alias.js";
import { readFileSync } from "node:fs";

test("native dialog submits through the selected binary", async () => {
  const source = readFileSync(new URL("./main.js", import.meta.url), "utf8")
    .replace(/^import .*;\n/gm, "").replace("export default class NameTerminal", "class NameTerminal");
  const factory = new Function("View", "terminals", "hc", "exec", "home", "normalizeAlias", "aliasTaken", "formatHcError", "saveName", "chooseTarget", source + "\nreturn NameTerminal;");
  const calls = [];
  const rows = [live("t1")];
  const ViewClass = factory(class {}, () => rows, async (args) => { calls.push(["hc", args]); return { code: 0 }; },
    async (cmd, args) => { calls.push([cmd, args]); return { code: 0 }; }, () => "/h",
    normalizeAlias, aliasTaken, formatHcError, saveName, chooseTarget);
  const view = new ViewClass();
  view.address = "t1";
  view.state = { kind: "editing" };
  view.input = { value: () => "writer", focus() {} };
  view.dismiss = () => {};
  const tasks = [];
  view.submit({ notify() {}, spawn: (task) => tasks.push(task({ notify() {} })) });
  await Promise.all(tasks);
  assert.deepEqual(calls[0], ["/h/.local/bin/namedrop-app", ["--version"]]);
});

const live = (address, extra = {}) => ({ address, state: "live", labels: { "herdr.space": "work" }, ...extra });
const missing = (command) => new Error(`herdr.exec: cannot run ${command}: No such file or directory (os error 2)`);

operation("props target wins and binary receives exact agent arguments", async () => {
  const calls = [];
  const rows = [live("t1"), live("t2", { status: "working" })];
  assert.equal(chooseTarget({ terminal: "t2" }, { terminal: "t1" }), "t2");
  const result = await saveName({ target: "t2", alias: "reviewer", terminals: () => rows,
    home: () => "/home/test", exec: async (cmd, args) => { calls.push([cmd, args]); return { code: 0 }; }, hc: async () => assert.fail("hc fallback") });
  assert.deepEqual(result, { ok: true });
  assert.deepEqual(calls, [
    ["/home/test/.local/bin/namedrop-app", ["--version"]],
    ["/home/test/.local/bin/namedrop-app", ["--host", "hc", "--hc-terminal", "t2", "--hc-space", "work", "name", "reviewer", "--type", "agent"]],
  ]);
});

operation("missing stable launcher uses PATH and shell type", async () => {
  const calls = [];
  const result = await saveName({ target: "t1", alias: "writer", terminals: () => [live("t1", { labels: {}, program: { name: "shell" } })], home: () => "/h",
    exec: async (cmd, args) => { calls.push([cmd, args]); if (cmd.startsWith("/")) throw missing(cmd); return { code: 0 }; }, hc: async () => assert.fail("hc fallback") });
  assert.deepEqual(result, { ok: true });
  assert.deepEqual(calls[2], ["namedrop", ["--host", "hc", "--hc-terminal", "t1", "--hc-space", "headless", "name", "writer", "--type", "terminal"]]);
});

operation("both missing candidates alone enable label fallback", async () => {
  const calls = [];
  const result = await saveName({ target: "t1", alias: "writer", terminals: () => [live("t1")], home: () => "/h",
    exec: async (cmd) => { throw missing(cmd); }, hc: async (args) => { calls.push(args); return { code: 0 }; } });
  assert.deepEqual(result, { ok: true });
  assert.deepEqual(calls, [["label", "t1", "namedrop.name=writer"]]);
});

operation("nonzero version and operational failures never write labels", async () => {
  for (const code of [1, 65]) {
    const result = await saveName({ target: "t1", alias: "writer", terminals: () => [live("t1")], home: () => "/h",
      exec: async () => ({ code, stderr: "bad version" }), hc: async () => assert.fail("hc fallback") });
    assert.match(result.error, /bad version|exit code/);
  }
  const result = await saveName({ target: "t1", alias: "writer", terminals: () => [live("t1")], home: () => "/h",
    exec: async (_cmd, args) => args[0] === "--version" ? { code: 0 } : { code: 65, stdout: '{"error":{"message":"already owned"}}' }, hc: async () => assert.fail("hc fallback") });
  assert.equal(result.error, "already owned");
});

operation("submit checks the fresh selected target", async () => {
  const result = await saveName({ target: "t1", alias: "writer", terminals: () => [live("t1", { state: "finished" })], home: () => "/h",
    exec: async () => assert.fail("exec"), hc: async () => assert.fail("hc") });
  assert.match(result.error, /no longer live/);
});
