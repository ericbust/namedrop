import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

 test("root keys preserve naming and roster and open three popup mods", () => {
  const keys = JSON.parse(readFileSync(new URL("../../keys.json", import.meta.url), "utf8")).global;
  assert.equal(keys["alt-n"], "mod.open:namedrop-name");
  assert.equal(keys["alt-shift-s"], "mod.open:namedrop");
  assert.equal(keys["alt-shift-m"], "mod.open:namedrop-management");
  assert.equal(keys["alt-shift-d"], "mod.open:namedrop-doctor");
  assert.equal(keys["alt-shift-o"], "mod.open:namedrop-tour");
});

test("all five bindings avoid Herdr app 0.1.9 defaults on Linux and macOS", () => {
  const keys = JSON.parse(readFileSync(new URL("../../keys.json", import.meta.url), "utf8")).global;
  const bindings = ["alt-n", "alt-shift-s", "alt-shift-m", "alt-shift-d", "alt-shift-o"];
  for (const file of ["keys.json", "keys.macos.json"]) {
    const defaults = JSON.parse(readFileSync(new URL(`./fixtures/herdr-app-0.1.9-${file}`, import.meta.url), "utf8"));
    const occupied = new Set(Object.values(defaults).flat());
    for (const binding of bindings) {
      assert.ok(keys[binding], binding);
      assert.equal(occupied.has(binding), false, `${binding} collides in ${file}`);
    }
  }
});
