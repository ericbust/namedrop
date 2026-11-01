import assert from "node:assert/strict";
import test from "node:test";
import { roster } from "./roster.js";

function terminal(address, alias, status = null, labels = {}) {
  return {
    address,
    state: "live",
    status,
    labels: { "namedrop.name": alias, ...labels },
  };
}

test("sorts blocked, working, done and idle together, then no status by alias", () => {
  const terminals = [
    terminal("t1", "z-no-status"),
    terminal("t2", "z-idle", "idle"),
    terminal("t3", "z-working", "working"),
    terminal("t4", "z-blocked", "blocked"),
    terminal("t5", "a-done", "done"),
    terminal("t6", "a-working", "working"),
    terminal("t7", "a-no-status"),
    terminal("t8", "a-error", "error"),
  ];
  assert.deepEqual(roster(terminals, [], []).map((row) => row.alias), [
    "a-error", "z-blocked", "a-working", "z-working",
    "a-done", "z-idle", "a-no-status", "z-no-status",
  ]);
  assert.equal(terminals[0].address, "t1");
});

test("omits a terminal with a missing name label", () => {
  assert.deepEqual(roster([{ address: "t1", state: "live", labels: {} }], [], []), []);
});

test("omits a terminal with an empty name label", () => {
  assert.deepEqual(roster([terminal("t1", "")], [], []), []);
});

test("omits a finished terminal even if it has a name and status", () => {
  assert.deepEqual(roster([{ ...terminal("t1", "reviewer", "blocked"), state: "finished" }], [], []), []);
});

test("keeps a shell or Codex row with null status and no program", () => {
  assert.deepEqual(roster([terminal("t1", "codex")], [], []), [{
    alias: "codex", address: "t1", status: null, program: null, shown: false, where: null,
  }]);
});

test("a terminal that never reported has null status", () => {
  const t = terminal("t1", "shell");
  delete t.status;
  assert.equal(roster([t], [], [])[0].status, null);
});

test("shows error as blocked and preserves done and idle", () => {
  const rows = roster([
    terminal("t1", "failed", "error"),
    terminal("t2", "done", "done"),
    terminal("t3", "idle", "idle"),
  ], [], []);
  assert.deepEqual(rows.map((row) => row.status), ["blocked", "done", "idle"]);
});

test("keeps the program object for its name and icon", () => {
  const program = { name: "Claude Code", icon: "icons/claude.svg", tint: true };
  assert.equal(roster([{ ...terminal("t1", "reviewer"), program }], [], [])[0].program, program);
});

test("looks up space and tab names from their label ids", () => {
  const t = terminal("t1", "reviewer", null, { "herdr.space": "s1", "herdr.tab": "tab1" });
  const spaces = [{ id: "s1", name: "Namedrop", tabs: [{ id: "tab1", name: "Review" }] }];
  assert.equal(roster([t], [], spaces)[0].where, "Namedrop / Review");
});

test("a tab name belongs to its space even if another space has the same tab id", () => {
  const t = terminal("t1", "reviewer", null, { "herdr.space": "s2", "herdr.tab": "tab1" });
  const spaces = [
    { id: "s1", name: "First", tabs: [{ id: "tab1", name: "Wrong" }] },
    { id: "s2", name: "Second", tabs: [{ id: "tab1", name: "Right" }] },
  ];
  assert.equal(roster([t], [], spaces)[0].where, "Second / Right");
});

test("resolves machine-local labels against qualified remote space and tab ids", () => {
  const t = terminal("vps/t1", "reviewer", null, { "herdr.space": "s1", "herdr.tab": "tab1" });
  const spaces = [
    { id: "s1", name: "Local", tabs: [{ id: "tab1", name: "Local tab" }] },
    { id: "vps/s1", name: "Remote", tabs: [{ id: "vps/tab1", name: "Remote tab" }] },
  ];
  assert.equal(roster([t], [], spaces)[0].where, "Remote / Remote tab");
});

test("where is null without both labels or without matching names", () => {
  for (const labels of [{}, { "herdr.space": "s1" }, { "herdr.tab": "tab1" },
    { "herdr.space": "missing", "herdr.tab": "missing" }]) {
    assert.equal(roster([terminal("t1", "reviewer", null, labels)], [], [])[0].where, null);
  }
});

test("shown follows the pane terminal address, including panes in other tabs", () => {
  const rows = roster([terminal("t1", "shown"), terminal("t2", "hidden")], [
    { pane: "p1", terminal: "t1", tab: "other" },
    { pane: "p2", terminal: null },
  ], []);
  assert.equal(rows.find((row) => row.address === "t1").shown, true);
  assert.equal(rows.find((row) => row.address === "t2").shown, false);
});

test("an empty check-mode snapshot produces no rows", () => {
  assert.deepEqual(roster([], [], []), []);
});

test("stored binding supplies an alias to a live terminal without a label", () => {
  const live = { address: "t1", state: "live", labels: {}, status: "idle" };
  assert.equal(roster([live], [], [], { core: "c", rows: [{ alias: "writer", terminal: "t1", space: "headless", waiting_on: [] }] })[0].alias, "writer");
});
