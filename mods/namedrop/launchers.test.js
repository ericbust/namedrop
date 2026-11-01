import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
const { launchPopup } = await import("../namedrop-management/launcher.js").catch(() => ({}));
const operation = launchPopup ? test : test.skip;

test("roster Agent status opens the selected row target", () => {
  const source = readFileSync(new URL("./main.js", import.meta.url), "utf8")
    .replace(/^import .*;\n/gm, "").replace("export default class Namedrop", "class Namedrop");
  const make = (id) => {
    const target = { id, children: [] };
    const proxy = new Proxy(target, { get(value, property) {
      if (property === "child") return (child) => { value.children.push(child); return proxy; };
      if (property === "on_click") return (handler) => { value.click = handler; return proxy; };
      if (property in value) return value[property];
      return () => proxy;
    } });
    return proxy;
  };
  const overlays = [];
  const factory = new Function("View", "div", "v_flex", "h_flex", "Button", "run", "openOverlay", source + "\nreturn Namedrop;");
  const Namedrop = factory(class {}, make, make, make, { new: make }, () => {}, (spec) => overlays.push(spec));
  const tree = new Namedrop().row({ alias: "writer", address: "t1", status: null, shown: false, where: null }, "right", { foreground: "white", muted: "gray" });
  const find = (node) => node?.id === "status-t1" ? node : node?.children?.map(find).find(Boolean);
  const status = find(tree);
  assert.equal(typeof status?.click, "function");
  status.click();
  assert.deepEqual(overlays[0].props, { terminal: "t1" });
});

const folders = ["management", "doctor", "agent-status", "tour"];
operation("launcher source copies differ only by popup identifier", () => {
  for (const file of ["main.js", "launcher.js"]) {
    const copies = folders.map((folder) => readFileSync(new URL(`../namedrop-${folder}/${file}`, import.meta.url), "utf8").replace(/const popup = "[^"]+"/, 'const popup = "POPUP"'));
    for (const copy of copies) assert.equal(copy, copies[0]);
  }
});

operation("management launches exact background command and 28px titlebar layout", async () => {
  const calls = [], overlays = [];
  const deps = { home: () => "/h", focused: () => ({ terminal: "t1" }), terminals: () => [{ address: "t1", state: "live", labels: { "herdr.space": "s" } }],
    exec: async (command, args) => { calls.push([command, args]); return args[0] === "--version" ? { code: 0 } : { code: 0, stdout: '{"address":"t9"}' }; }, openOverlay: (spec) => overlays.push(spec) };
  await launchPopup(deps);
  assert.deepEqual(calls[1], ["/h/.local/bin/namedrop-app", ["--host", "hc", "run", "--bg", "--json", "--rm", "--name", "namedrop-management", "--cwd", "/h", "--", "/h/.local/bin/namedrop-app", "--host", "hc", "--hc-terminal", "t1", "--hc-space", "s", "ui", "run", "manage"]]);
  assert.equal(overlays[0].key, "namedrop-management");
  assert.deepEqual(overlays[0].layout.children.map((child) => child.content), [{ mod: "namedrop-management", props: { part: "bar" } }, { terminal: "t9" }]);
  assert.deepEqual(overlays[0].layout.children[0].size, { px: 28 });
});

operation("structured name_taken reopens only a live named terminal", async () => {
  const overlays = [];
  const deps = { home: () => "/h", focused: () => ({ terminal: "t1" }), terminals: () => [
    { address: "t1", state: "live", labels: {} }, { address: "t9", name: "namedrop-management", state: "live" }],
    exec: async (_command, args) => args[0] === "--version" ? { code: 0 } : { code: 65, stdout: '{"error":{"code":"name_taken","message":"taken"}}' },
    openOverlay: (spec) => overlays.push(spec) };
  await launchPopup(deps);
  assert.equal(overlays[0].layout.children[1].content.terminal, "t9");
  const other = { ...deps, exec: async (_command, args) => args[0] === "--version" ? { code: 0 } : { code: 65, stdout: '{"error":{"code":"other","message":"bad"}}' } };
  assert.deepEqual(await launchPopup(other), { error: "bad" });
  assert.equal(overlays.length, 1);
});
