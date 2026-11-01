import assert from "node:assert/strict";
import test from "node:test";
import { initialModel, transition } from "./model.js";

const row = (status, replies = [], terminal = "t1", space = "s") => ({ alias: "writer", terminal, space, agent_status: status, recent_replies: replies });
const snap = (...rows) => ({ core: "core", rows, state_directory: "/s" });
const reply = (id, at_ms = 1) => ({ message_id: id, from: "reviewer", at_ms });
function read(model, now, snapshot) { return transition(model, { type: "readOk", now, snapshot }); }

test("first read and newly appearing rows seed reply and status history", () => {
  let { model, effects } = read(initialModel(), 0, snap(row("blocked", [reply("r1")])));
  assert.equal(effects.some((effect) => effect.type === "toast"), false);
  ({ model, effects } = read(model, 1, snap(row("blocked", [reply("r1")]), row("blocked", [reply("r2")], "t2"))));
  assert.equal(effects.some((effect) => effect.type === "toast"), false);
});

test("continuing row receives blocked and reply toast with exact focus", () => {
  let { model } = read(initialModel(), 0, snap(row(null, [reply("r1")])));
  let result = read(model, 1, snap(row("blocked", [reply("r2", 2), reply("r1")])));
  assert.equal(result.effects.find((effect) => effect.type === "toast")?.message, "@writer is blocked");
  model = result.model;
  result = transition(model, { type: "toastTick", now: 1001, at: model.toastAt });
  assert.deepEqual(result.effects.find((effect) => effect.type === "toast")?.action, { name: "agent.focus", args: { terminal: "t1" } });
  assert.equal(result.effects.find((effect) => effect.type === "toast")?.message, "@reviewer replied to @writer");
  result = read(result.model, 1002, snap(row("blocked", [reply("r2", 2), reply("r1")])));
  assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
});

test("four updates collapse completely and later updates cannot postpone the aggregate", () => {
  let { model } = read(initialModel(), 0, snap(row("working")));
  let result = read(model, 1, snap(row("blocked")));
  model = result.model;
  result = read(model, 2, snap(row("blocked", [1, 2, 3, 4].map((id) => reply(`r${id}`, id)))));
  assert.equal(result.model.pending.length, 0);
  assert.equal(result.model.overflow, 4);
  result = read(result.model, 900, snap(row("blocked", [1, 2, 3, 4, 5].map((id) => reply(`r${id}`, id)))));
  assert.equal(result.model.pending.length, 0);
  assert.equal(result.model.overflow, 5);
  assert.equal(result.model.toastAt, 1001);
  result = transition(result.model, { type: "toastTick", now: 5000, at: 1001 });
  assert.deepEqual(result.effects.filter((effect) => effect.type === "toast"), [{
    type: "toast", message: "5 more updates", action: { name: "mod.open", args: { name: "namedrop" } },
  }]);
  assert.equal(result.model.overflow, 0);
  result = transition(result.model, { type: "toastTick", now: 5000, at: 1001 });
  assert.equal(result.effects.length, 0);
});

test("distinct read errors share notification gate", () => {
  let result = transition(initialModel(), { type: "readError", now: 0, error: "broken" });
  assert.equal(result.effects.find((effect) => effect.type === "toast")?.message, "broken");
  result = transition(result.model, { type: "readError", now: 1, error: "broken" });
  assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
});

test("unbound stored rows never create a focus notification", () => {
  let { model } = read(initialModel(), 0, snap(row("working", [], null)));
  const result = read(model, 1, snap(row("blocked", [reply("r1")], null)));
  assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
});

for (const change of ["disappearance", "core replacement"]) {
  test(`${change} prunes histories and returning rows seed again`, () => {
    let { model } = read(initialModel(), 0, snap(row("working", [reply("old", 100)])));
    const absent = change === "disappearance" ? snap() : { ...snap(row("working")), core: "other" };
    ({ model } = read(model, 1, absent));
    assert.equal(Object.keys(model.seen).length, absent.rows.length);
    const result = read(model, 2, snap(row("blocked", [reply("new", 101)])));
    assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
  });
}

test("all 300 continuing addresses retain their notification histories", () => {
  const rows = Array.from({ length: 300 }, (_, i) => row("working", [], `t${i}`));
  let { model } = read(initialModel(), 0, snap(...rows));
  assert.equal(Object.keys(model.seen).length, 300);
  rows[0] = row("blocked", [], "t0");
  const result = read(model, 1, snap(...rows));
  assert.equal(result.effects.find((effect) => effect.type === "toast")?.action.args.terminal, "t0");
});

test("restoring an older timeline seeds it and accepts the next reply", () => {
  let { model } = read(initialModel(), 0, snap(row("working", [reply("future", 100)])));
  let result = read(model, 1, snap(row("working", [reply("restored", 10)])));
  assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
  result = read(result.model, 2, snap(row("working", [reply("fresh", 11), reply("restored", 10)])));
  assert.equal(result.effects.filter((effect) => effect.type === "toast").length, 1);
});

test("four immediately eligible updates emit only one aggregate", () => {
  const first = read(initialModel(), 0, snap(row("working")));
  const result = read(first.model, 1, snap(row("working", [1, 2, 3, 4].map((id) => reply(`r${id}`, id)))));
  assert.deepEqual(result.effects, [{ type: "toast", message: "4 more updates", action: { name: "mod.open", args: { name: "namedrop" } } }]);
  assert.deepEqual(result.model.pending, []);
  assert.equal(result.model.overflow, 0);
});

test("a restored address seeds its historical blocked status", () => {
  let { model } = read(initialModel(), 0, snap(row("working", [reply("future", 100)])));
  let result = read(model, 1, snap(row("blocked", [reply("restored", 10)])));
  assert.equal(result.effects.some((effect) => effect.type === "toast"), false);
  result = read(result.model, 2, snap(row("blocked", [reply("fresh", 11), reply("restored", 10)])));
  assert.equal(result.effects.find((effect) => effect.type === "toast")?.message, "@reviewer replied to @writer");
});

for (const fresh of [false, true]) {
  test(`readError preserves reply history when recovery ${fresh ? "includes a new reply" : "has only historical replies"}`, () => {
    let { model } = read(initialModel(), 0, snap(row("working", [reply("old", 10)])));
    ({ model } = transition(model, { type: "readError", now: 1000, error: "read failed" }));
    const replies = fresh ? [reply("new", 11), reply("old", 10)] : [reply("old", 10)];
    const result = read(model, 2000, snap(row("working", replies)));
    assert.deepEqual(result.effects.filter((effect) => effect.type === "toast").map((effect) => effect.message),
      fresh ? ["@reviewer replied to @writer"] : []);
  });
}
