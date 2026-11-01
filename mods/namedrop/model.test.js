import assert from "node:assert/strict";
import test from "node:test";
import { initialModel, transition, watchedStoreFile } from "./model.js";
import { roster } from "./roster.js";

function step(model, type, now, extra = {}) { return transition(model, { type, now, ...extra }); }

test("one read is reserved and dirty changes coalesce at the 500ms floor", () => {
  let { model, effects } = step(initialModel(), "wake", 0);
  assert.deepEqual(effects, [{ type: "read" }]);
  ({ model, effects } = step(model, "wake", 1));
  assert.deepEqual(effects, []);
  ({ model, effects } = step(model, "readOk", 100, { snapshot: { core: null, rows: [], state_directory: "/state" } }));
  assert.deepEqual(effects, [{ type: "timer", delay: 400 }]);
  ({ model, effects } = step(model, "wake", 499));
  assert.deepEqual(effects, [{ type: "timer", delay: 1 }]);
  ({ model, effects } = step(model, "wake", 500));
  assert.deepEqual(effects, [{ type: "read" }]);
  ({ model, effects } = step(model, "wake", 500));
  assert.deepEqual(effects, []);
  ({ model, effects } = step(model, "readOk", 999, { snapshot: { core: null, rows: [], state_directory: "/state" } }));
  assert.deepEqual(effects, [{ type: "timer", delay: 1 }]);
  ({ model, effects } = step(model, "wake", 1000));
  assert.deepEqual(effects, [{ type: "read" }]);
});

test("only absolute store basenames trigger refresh", () => {
  assert.equal(watchedStoreFile("/state/namedrop.sqlite3-wal"), true);
  assert.equal(watchedStoreFile("namedrop.sqlite3"), false);
  assert.equal(watchedStoreFile("/state/other.sqlite3"), false);
});

test("stored wait merges with fresh terminal status and ignores unbound rows", () => {
  const live = [{ address: "t1", state: "live", labels: { "namedrop.name": "writer" }, status: "working", program: { name: "Claude" } }];
  const rows = roster(live, [], [], { core: "c", rows: [
    { alias: "writer", terminal: "t1", space: "headless", waiting_on: [{ alias: "reviewer" }] },
    { alias: "orphan", terminal: null, space: "headless", waiting_on: [] },
  ] });
  assert.equal(rows.length, 1);
  assert.equal(rows[0].status, "working");
  assert.equal(rows[0].where, "waiting on @reviewer");
});

test("failed reads reserve ensure at most once per 30 seconds", () => {
  let { model } = step(initialModel(), "wake", 0);
  let result = step(model, "readError", 10, { error: "bad" });
  assert.equal(result.effects.filter((effect) => effect.type === "ensure").length, 1);
  model = result.model;
  ({ model } = step(model, "wake", 500));
  result = step(model, "readError", 510, { error: "bad" });
  assert.equal(result.effects.filter((effect) => effect.type === "ensure").length, 0);
});
