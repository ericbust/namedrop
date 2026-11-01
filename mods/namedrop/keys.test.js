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
