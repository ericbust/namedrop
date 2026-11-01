import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { initialModel, transition, watchedStoreFile } from "./model.js";

const source = readFileSync(new URL("./main.js", import.meta.url), "utf8")
  .replace(/^import .*;\n/gm, "").replace("export default class Namedrop", "class Namedrop");
const snapshot = { core: "c", rows: [], state_directory: "/state" };
const ok = { code: 0, stdout: JSON.stringify(snapshot) };
const failure = { code: 65, stdout: JSON.stringify({ error: { message: "raw database refusal\nwith detail" } }), stderr: "" };
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };

function harness({ execute = async () => ok, watching = () => new Promise(() => {}) } = {}) {
  let now = 0;
  const calls = [], notices = [], timers = [], watches = [], errors = [], buttons = new Map();
  const make = (id) => {
    const proxy = new Proxy({}, { get(_target, key) {
      if (key === "when") return (condition, fn) => { if (condition) fn(proxy); return proxy; };
      if (key === "on_click") return (fn) => { buttons.set(id, fn); return proxy; };
      return () => proxy;
    } });
    return proxy;
  };
  const dependencies = {
    View: class {}, initialModel, transition, watchedStoreFile,
    exec: async (command, args) => { calls.push({ command, args, now }); return execute(command, args); },
    home: () => "/private", watchFolder: (directory) => { watches.push(directory); return watching(directory); },
    notify: (notice) => notices.push(notice), Date: { now: () => now },
    div: make, v_flex: make, h_flex: make, Button: { new: make },
    theme: () => ({}), terminals: () => [], panes: () => [], spaces: () => [], roster: () => [], placement: () => null, props: () => ({}),
  };
  const Namedrop = new Function(...Object.keys(dependencies), source + "\nreturn Namedrop;")(...Object.values(dependencies));
  const cx = { notify() {}, spawn(fn) { Promise.resolve().then(() => fn(cx)).catch((error) => errors.push(error)); },
    timer: { after(delay, fn) { timers.push({ at: now + delay, fn }); } } };
  const view = new Namedrop();
  const settle = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); assert.deepEqual(errors, []); };
  const fire = async (timer, time = timer.at) => { timers.splice(timers.indexOf(timer), 1); now = time; timer.fn(cx); await settle(); };
  return { view, cx, calls, notices, timers, watches, buttons, settle, fire, setTime(time) { now = time; },
    ensures: () => calls.filter((call) => call.args.includes("--ensure")),
    async start() { view.init({}, cx); await settle(); },
    async retry(time) { now = time; view.render(); buttons.get("retry-roster")({}, cx); await settle(); },
  };
}

test("startup and failed reads share the ensure reservation and preserve errors", async () => {
  const pending = deferred();
  const h = harness({ execute: async (_command, args) => args.includes("--ensure") ? pending.promise : args.includes("roster") ? failure : { code: 0 } });
  await h.start();
  assert.equal(h.ensures().length, 1);
  assert.equal(h.view.model.error, "raw database refusal\nwith detail");
  assert.equal(h.notices[0]?.title, "raw database refusal\nwith detail");
  assert.equal(h.notices[0]?.level, "error");
  await h.retry(30000);
  assert.equal(h.ensures().length, 1, "pending ensure cannot overlap after thirty seconds");
  pending.resolve({ code: 0 });
  await h.settle();
  await h.retry(30500);
  assert.equal(h.ensures().length, 2);
});

test("completed startup ensure cannot repeat before the exact thirty-second boundary", async () => {
  const h = harness({ execute: async (_command, args) => args.includes("roster") ? failure : { code: 0 } });
  await h.start();
  await h.retry(29500);
  assert.equal(h.ensures().length, 1);
  await h.retry(30000);
  assert.equal(h.ensures().length, 2);
});

test("rejected ensure releases its reservation without escaping the adapter", async () => {
  const h = harness({ execute: async (_command, args) => {
    if (args.includes("--ensure")) throw new Error("ensure transport failure");
    return args.includes("roster") ? failure : { code: 0 };
  } });
  await h.start();
  await h.retry(30000);
  assert.equal(h.ensures().length, 2);
});

test("error-row Retry reprobes binary discovery before running roster", async () => {
  let blocked = true;
  const h = harness({ execute: async (command, args) => {
    assert.equal(typeof command, "string");
    if (args.includes("--version") && blocked) throw new Error("permission denied");
    return args.includes("roster") ? ok : { code: 0 };
  } });
  await h.start();
  assert.equal(h.view.install, "error");
  blocked = false;
  await h.retry(1000);
  assert.equal(h.calls.filter((call) => call.args.includes("--version")).length, 2);
  assert.equal(h.view.install, "available");
  assert.equal(h.view.model.error, null);
  assert.equal(h.ensures().length, 1);
});

test("missing and recreated watch directories recover through bounded scheduled retry", async () => {
  let exists = false;
  let pending = deferred();
  const h = harness({ watching: async () => { if (!exists) throw new Error("missing directory"); return pending.promise; } });
  await h.start();
  assert.equal(h.watches.length, 1);
  assert.equal(h.timers.length, 1);
  const first = h.timers[0];
  assert.ok(first.at >= 500 && first.at <= 30000);
  await h.fire(first, 60000);
  assert.equal(h.watches.length, 2, "a delayed retry must not catch up in a loop");
  assert.equal(h.timers.length, 1);
  exists = true;
  await h.fire(h.timers[0]);
  assert.equal(h.watches.length, 3);
  assert.equal(h.timers.length, 0);
  const reads = h.calls.filter((call) => call.args.includes("roster")).length;
  pending.resolve(["/state/namedrop.sqlite3-wal"]);
  pending = deferred();
  await h.settle();
  assert.equal(h.calls.filter((call) => call.args.includes("roster")).length, reads + 1);
  exists = false;
  pending.reject(new Error("directory removed"));
  await h.settle();
  assert.equal(h.timers.length, 1);
  exists = true;
  pending = deferred();
  await h.fire(h.timers[0]);
  assert.equal(h.timers.length, 0);
});

test("a stale watch retry cannot revive a replaced directory", async () => {
  const h = harness({ watching: async (directory) => {
    if (directory === "/state") throw new Error("missing directory");
    return new Promise(() => {});
  } });
  await h.start();
  assert.equal(h.timers.length, 1);
  const stale = h.timers[0];
  h.view.watch("/new-state", h.cx);
  await h.settle();
  await h.fire(stale);
  assert.deepEqual(h.watches, ["/state", "/new-state"]);
  assert.equal(h.timers.length, 0);
});

test("binary stdout error.message reaches the error row verbatim", async () => {
  const h = harness({ execute: async (_command, args) => args.includes("roster") ? failure : { code: 0 } });
  await h.start();
  assert.equal(h.view.model.error, "raw database refusal\nwith detail");
});

test("error notices retain the native error notification level", async () => {
  const h = harness({ execute: async (_command, args) => args.includes("roster") ? failure : { code: 0 } });
  await h.start();
  assert.equal(h.notices[0]?.level, "error");
});
