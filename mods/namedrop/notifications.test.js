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

test("four pending updates collapse and delayed tick emits only one", () => {
  let { model } = read(initialModel(), 0, snap(row("working")));
  ({ model } = read(model, 1, snap(row("blocked", [reply("r1"), reply("r2"), reply("r3"), reply("r4")]))));
  assert.ok(model.pending.length <= 3);
  assert.ok(model.overflow >= 2);
  const result = transition(model, { type: "toastTick", now: 5000, at: model.toastAt });
  assert.equal(result.effects.filter((effect) => effect.type === "toast").length, 1);
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
