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
  const calls = [], notices = [], timers = [], watches = [], watchTimes = [], errors = [], buttons = new Map();
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
    home: () => "/private", watchFolder: (directory) => { watches.push(directory); watchTimes.push(now); return watching(directory); },
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
  return { view, cx, calls, notices, timers, watches, watchTimes, buttons, settle, fire, setTime(time) { now = time; },
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
  h.setTime(62500);
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

const rosterReads = (h) => h.calls.filter((call) => call.args.includes("roster"));
async function advance(h, until) {
  let count = 0;
  while (h.timers.some((timer) => timer.at <= until)) {
    assert.ok(++count < 200, "timer loop must remain bounded");
    await h.fire(h.timers.reduce((a, b) => a.at < b.at ? a : b));
  }
  h.setTime(until);
}

test("sixty seconds of watch rejections perform at most three roster execs", async () => {
  const h = harness({ watching: async () => { throw new Error("missing directory"); } });
  await h.start();
  await advance(h, 60000);
  assert.ok(rosterReads(h).length <= 3, `${rosterReads(h).length} roster execs`);
});

test("watch retries back off at 1, 2, 4, 8, 16 and 30 seconds without roster wakes", async () => {
  const h = harness({ watching: async () => { throw new Error("missing directory"); } });
  await h.start();
  let now = 0;
  for (const delay of [1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
    assert.equal(h.timers.length, 1);
    assert.equal(h.timers[0].at, now + delay);
    now += delay;
    await h.fire(h.timers[0]);
    assert.equal(rosterReads(h).length, 1, "retry must not wake roster");
  }
});

test("a recovered watch waits for its first change then refreshes once even for irrelevant paths", async () => {
  let pending;
  const h = harness({ watching: () => pending ? pending.promise : Promise.reject(new Error("missing")) });
  await h.start();
  await h.fire(h.timers[0]);
  pending = deferred();
  await h.fire(h.timers[0]);
  assert.equal(rosterReads(h).length, 1, "pending watch is not an attachment acknowledgment");
  const first = pending; pending = deferred();
  first.resolve(["/state/unrelated"]);
  await h.settle();
  assert.equal(rosterReads(h).length, 2, "first fulfillment recovers despite basename filter");
  h.setTime(10000);
  const next = pending; pending = deferred(); next.resolve(["/state/unrelated"]);
  await h.settle();
  assert.equal(rosterReads(h).length, 2, "later unrelated changes do not refresh");
  pending.reject(new Error("removed again"));
  await h.settle();
  assert.equal(h.timers.length, 1);
  assert.equal(h.timers[0].at, 11000, "success resets watch backoff");
});

for (const outcome of ["resolve", "reject"]) {
  test(`stale watch promise ${outcome} cannot wake or retry a replaced directory`, async () => {
    const stale = deferred();
    const h = harness({ watching: (directory) => directory === "/state" ? stale.promise : new Promise(() => {}) });
    await h.start();
    h.view.watch("/new-state", h.cx);
    await h.settle();
    if (outcome === "resolve") stale.resolve(["/state/namedrop.sqlite3"]);
    else stale.reject(new Error("stale directory"));
    await h.settle();
    assert.deepEqual(h.watches, ["/state", "/new-state"]);
    assert.equal(rosterReads(h).length, 1);
    assert.equal(h.timers.length, 0);
  });
}

for (const status of ["started", "running"]) {
  for (const order of ["read-first", "ensure-first"]) {
    test(`${status} ensure recovers failed initial read once in ${order} order`, async () => {
      const read = deferred(), ensure = deferred();
      let reads = 0;
      const h = harness({ execute: async (_command, args) => {
        if (args.includes("--ensure")) return ensure.promise;
        if (args.includes("roster")) return ++reads === 1 ? read.promise : ok;
        return { code: 0 };
      } });
      await h.start();
      const success = { code: 0, stdout: JSON.stringify({ status, pid: 123 }) };
      if (order === "read-first") { read.resolve(failure); await h.settle(); ensure.resolve(success); }
      else { ensure.resolve(success); await h.settle(); read.resolve(failure); }
      await h.settle();
      await advance(h, 1000);
      assert.equal(reads, 2);
      assert.equal(h.view.model.error, null);
      assert.deepEqual(h.watches, ["/state"]);
      assert.equal(h.ensures().length, 1);
      await advance(h, 60000);
      assert.equal(reads, 2);
    });
  }
}

for (const [name, result] of [
  ["inactive", { code: 0, stdout: '{"status":"inactive","pid":null}' }],
  ["malformed", { code: 0, stdout: 'not json' }],
  ["nonzero", { code: 65, stdout: '{"status":"running","pid":123}' }],
  ["thrown", new Error("ensure failed")],
]) {
  test(`${name} ensure does not automatically recover a failed read`, async () => {
    const h = harness({ execute: async (_command, args) => {
      if (args.includes("--ensure")) { if (result instanceof Error) throw result; return result; }
      return args.includes("roster") ? failure : { code: 0 };
    } });
    await h.start(); await advance(h, 60000);
    assert.equal(rosterReads(h).length, 1);
    assert.equal(h.ensures().length, 1);
    assert.equal(h.view.model.error, "raw database refusal\nwith detail");
  });
}

test("successful initial read needs no duplicate read when ensure completes", async () => {
  const ensure = deferred();
  const h = harness({ execute: async (_command, args) => args.includes("--ensure") ? ensure.promise : args.includes("roster") ? ok : { code: 0 } });
  await h.start();
  ensure.resolve({ code: 0, stdout: '{"status":"started","pid":123}' });
  await h.settle(); await advance(h, 60000);
  assert.equal(rosterReads(h).length, 1);
});

test("a recovery read failing after thirty seconds cannot create an ensure loop", async () => {
  const ensure = deferred(), recovery = deferred();
  let reads = 0;
  const h = harness({ execute: async (_command, args) => {
    if (args.includes("--ensure")) return ensure.promise;
    if (args.includes("roster")) return ++reads === 1 ? failure : recovery.promise;
    return { code: 0 };
  } });
  await h.start();
  ensure.resolve({ code: 0, stdout: '{"status":"running","pid":123}' });
  await h.settle(); await advance(h, 500);
  assert.equal(reads, 2);
  h.setTime(31000); recovery.resolve(failure);
  await h.settle(); await advance(h, 90000);
  assert.equal(reads, 2);
  assert.equal(h.ensures().length, 1);
});

test("a slow initial failure consumes successful ensure without reserving another ensure", async () => {
  const first = deferred(), ensure = deferred();
  let reads = 0;
  const h = harness({ execute: async (_command, args) => {
    if (args.includes("--ensure")) return ensure.promise;
    if (args.includes("roster")) return ++reads === 1 ? first.promise : ok;
    return { code: 0 };
  } });
  await h.start();
  ensure.resolve({ code: 0, stdout: '{"status":"running","pid":123}' });
  await h.settle();
  h.setTime(31000); first.resolve(failure);
  await h.settle(); await advance(h, 90000);
  assert.equal(reads, 2);
  assert.equal(h.ensures().length, 1);
  assert.equal(h.view.model.error, null);
});


test("a pending retry refreshes backoff writes exactly once after 500ms without a change", async () => {
  let exists = false;
  let current = snapshot;
  let pending = deferred();
  const h = harness({
    watching: () => exists ? pending.promise : Promise.reject(new Error("missing")),
    execute: async (_command, args) => args.includes("roster") ? { code: 0, stdout: JSON.stringify(current) } : { code: 0 },
  });
  await h.start();
  await advance(h, 60000);
  assert.ok(rosterReads(h).length <= 3);
  const reads = rosterReads(h).length;
  current = { ...snapshot, rows: [{ alias: "written-during-backoff" }] };
  exists = true;
  await advance(h, 61000);
  assert.equal(h.watchTimes.at(-1), 61000);
  await advance(h, 61499);
  assert.equal(rosterReads(h).length, reads);
  await advance(h, 61500);
  assert.equal(rosterReads(h).length, reads + 1, "pending retry must refresh at the grace boundary");
  assert.deepEqual(h.view.model.snapshot, current);
  await advance(h, 70000);
  assert.equal(rosterReads(h).length, reads + 1);
  const attached = pending; pending = deferred();
  attached.resolve(["/state/unrelated"]);
  await h.settle();
  assert.equal(rosterReads(h).length, reads + 1, "irrelevant fulfillment cannot repeat recovery");
  pending.reject(new Error("removed again"));
  await h.settle();
  pending = deferred();
  await advance(h, 70999);
  assert.equal(h.watchTimes.at(-1), 70000);
  await advance(h, 71000);
  assert.equal(h.watchTimes.at(-1), 71000, "grace recovery resets backoff to one second");
});
