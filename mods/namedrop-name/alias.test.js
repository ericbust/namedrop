import assert from "node:assert/strict";
import test from "node:test";
import { normalizeAlias, aliasTaken } from "./alias.js";

const invalidCharacters = "aliases can contain only ASCII letters, digits, spaces, hyphens, or underscores";
const invalidAlias = "alias must be lowercase ASCII kebab-case with no leading or trailing hyphen";

test("normalizes Code Reviewer to code-reviewer", () => {
  assert.deepEqual(normalizeAlias("Code Reviewer"), { alias: "code-reviewer" });
});

test("keeps lowercase kebab-case and digits", () => {
  assert.deepEqual(normalizeAlias("reviewer-42"), { alias: "reviewer-42" });
});

test("collapses separators and removes leading and trailing separators", () => {
  assert.deepEqual(normalizeAlias("  --Code__  Reviewer--_ "), { alias: "code-reviewer" });
});

test("normalizes exactly Rust ASCII whitespace", () => {
  assert.deepEqual(normalizeAlias("A\tB\nC\fD\rE F"), { alias: "a-b-c-d-e-f" });
});

for (const input of ["", " ", "---", "___", " \t\n\f\r_- "]) {
  test(`rejects an empty normalized alias from ${JSON.stringify(input)} with the Rust validation error`, () => {
    assert.deepEqual(normalizeAlias(input), { error: invalidAlias });
  });
}

for (const input of ["@reviewer", "code.reviewer", "code/reviewer", "reviewer!", "café", "审阅", "agent😀", "a\u00a0b", "a\vb", "a\0b"]) {
  test(`rejects unsupported characters in ${JSON.stringify(input)} with the Rust character error`, () => {
    assert.deepEqual(normalizeAlias(input), { error: invalidCharacters });
  });
}

test("invalid characters win over an otherwise empty normalized alias", () => {
  assert.deepEqual(normalizeAlias(" -- @ -- "), { error: invalidCharacters });
});

function terminal(address, alias, space, state = "live") {
  const labels = { "namedrop.name": alias };
  if (space !== undefined) labels["herdr.space"] = space;
  return { address, labels, state };
}

test("another live terminal in the same space takes the alias", () => {
  assert.equal(aliasTaken([terminal("t1", "coder", "s1"), terminal("t2", "reviewer", "s1")], "reviewer", "t1"), true);
});

test("a terminal in another space does not take the alias", () => {
  assert.equal(aliasTaken([terminal("t1", "coder", "s1"), terminal("t2", "reviewer", "s2")], "reviewer", "t1"), false);
});

test("the target itself does not take its alias", () => {
  assert.equal(aliasTaken([terminal("t1", "reviewer", "s1")], "reviewer", "t1"), false);
});

test("finished terminals do not take the alias", () => {
  assert.equal(aliasTaken([terminal("t1", "coder", "s1"), terminal("t2", "reviewer", "s1", "finished")], "reviewer", "t1"), false);
});

test("another live terminal without a space takes the alias for an unlabelled target", () => {
  assert.equal(aliasTaken([terminal("t1", "coder"), terminal("t2", "reviewer")], "reviewer", "t1"), true);
});

test("missing space does not conflict with a labelled space in either direction", () => {
  const terminals = [terminal("t1", "coder"), terminal("t2", "reviewer", "s1")];
  assert.equal(aliasTaken(terminals, "reviewer", "t1"), false);
  assert.equal(aliasTaken(terminals, "coder", "t2"), false);
});

test("another alias in the same space does not conflict", () => {
  assert.equal(aliasTaken([terminal("t1", "coder", "s1"), terminal("t2", "writer", "s1")], "reviewer", "t1"), false);
});
