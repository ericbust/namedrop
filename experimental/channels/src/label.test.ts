import assert from "node:assert/strict";
import { test } from "node:test";
import { foldLabel } from "./label.ts";

test("foldLabel mirrors WorkspaceLabel::from_herdr: ASCII letters, digits, - _ . kept lowercased, every other run one dash", () => {
  const cases: [string, string][] = [
    ["alpha", "alpha"],
    ["Alpha Project", "alpha-project"],
    ["a__b.c-d", "a__b.c-d"],
    ["a  --  b", "a---b"],
    ["a\tb/c", "a-b-c"],
    ["  spaced  ", "spaced"],
    ["!!!ends with!!!", "ends-with"],
    ["Ünïcode", "n-code"],
    ["日本語", ""],
    ["日本語 2024", "2024"],
    ["🚀 launch", "launch"],
    ["All", "all"],
    ["", ""],
    ["x".repeat(70), "x".repeat(64)],
    [`${"y".repeat(63)} zz`, `${"y".repeat(63)}-`],
  ];
  for (const [label, folded] of cases)
    assert.equal(foldLabel(label), folded, JSON.stringify(label));
});
