import assert from "node:assert/strict";
import { test } from "node:test";
import { panelWidths, readPanelWidths } from "./panelWidths.ts";

test("saved panel preferences accept only finite positive widths", () => {
  for (const raw of [null, "broken", "null", "[]", '{"left":"240","right":-1}'])
    assert.deepEqual(readPanelWidths(raw), {});
  assert.deepEqual(readPanelWidths('{"left":300,"right":420}'), {
    left: 300,
    right: 420,
  });
  assert.deepEqual(readPanelWidths('{"left":1e999,"right":400}'), {
    right: 400,
  });
});
test("panel layout reserves 320px for messages and restores preferences after shrinking", () => {
  const preferences = { left: 350, right: 620 };
  for (const viewport of [861, 1024, 1440]) {
    const widths = panelWidths(preferences, viewport, true);
    assert.ok(widths.left >= 180 && widths.right >= 280);
    assert.ok(viewport - widths.left - widths.right >= 320);
  }
  assert.deepEqual(panelWidths(preferences, 1440, true), preferences);
  assert.deepEqual(panelWidths({}, 1024, true), { left: 220, right: 360 });
});
