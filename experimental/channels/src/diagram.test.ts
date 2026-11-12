import assert from "node:assert/strict";
import test from "node:test";
import { isDiagram, type Palette } from "./diagram.ts";
import {
  diagramConfig,
  drawingKey,
  imageAddress,
  isDark,
} from "./diagramConfig.ts";
import { type Block, parseMarkdown } from "./markdown.ts";

const light: Palette = {
  page: "#fdfdff",
  raised: "#f3f4f7",
  hover: "#ebecf1",
  selected: "#e0e5fe",
  line: "#e2e3e7",
  lineStrong: "#caccd1",
  text: "#1b1e27",
  muted: "#5d606b",
  font: 'system-ui, "Segoe UI", sans-serif',
};
const dark: Palette = {
  ...light,
  page: "#15161c",
  raised: "#1c1d25",
  text: "#e8e9ef",
};
const code = (source: string) => {
  const block = parseMarkdown(source).find((found) => found.kind === "code");
  assert.ok(block, source);
  return block as Extract<Block, { kind: "code" }>;
};

test("a fence tagged mermaid is a diagram, whatever its case or what follows the tag", () => {
  for (const fence of ["```mermaid", "~~~Mermaid", "```MERMAID title=plan"])
    assert.equal(isDiagram(code(`${fence}\ngraph TD\n`)), true, fence);
  for (const fence of ["```", "```js", "```mermaidx", "```text mermaid"])
    assert.equal(isDiagram(code(`${fence}\ngraph TD\n`)), false, fence);
});

test("a diagram cannot change the settings that keep it safe or on the page's look", () => {
  const config = diagramConfig(light);
  assert.equal(config.securityLevel, "strict");
  assert.equal(config.htmlLabels, false);
  assert.equal(config.suppressErrorRendering, true);
  assert.equal(config.startOnLoad, false);
  // Mermaid drops these keys from a diagram's own directives and front matter.
  for (const locked of [
    "secure",
    "securityLevel",
    "htmlLabels",
    "suppressErrorRendering",
    "theme",
    "themeVariables",
    "themeCSS",
    "darkMode",
    "fontFamily",
  ])
    assert.ok(config.secure.includes(locked), locked);
  assert.ok(
    !config.secure.includes("layout"),
    "a diagram may still ask for another layout",
  );
});

test("the drawing takes its colors and font from the page", () => {
  assert.equal(isDark(light), false);
  assert.equal(isDark(dark), true);
  const config = diagramConfig(dark);
  assert.equal(config.theme, "base");
  assert.equal(config.darkMode, true);
  assert.equal(config.themeVariables.darkMode, true);
  assert.equal(config.themeVariables.primaryTextColor, dark.text);
  assert.equal(config.themeVariables.primaryColor, dark.page);
  assert.equal(config.themeVariables.background, dark.raised);
  assert.equal(config.fontFamily, dark.font);
  assert.equal(diagramConfig(light).darkMode, false);
});

test("the same source is drawn again when the page's look changes", () => {
  const source = "graph TD\n  A --> B\n";
  assert.equal(drawingKey(source, light), drawingKey(source, { ...light }));
  assert.notEqual(drawingKey(source, light), drawingKey(source, dark));
  assert.notEqual(
    drawingKey(source, light),
    drawingKey(`${source}  B --> C\n`, light),
  );
});

test("the picture's address is an SVG data address that survives any text", () => {
  const svg =
    '<svg xmlns="http://www.w3.org/2000/svg"><text>50% of "é" #1 ✓</text></svg>';
  const address = imageAddress(svg);
  assert.ok(address.startsWith("data:image/svg+xml;charset=utf-8,"));
  assert.ok(!/[#"<> ]/.test(address.slice(address.indexOf(",") + 1)));
  assert.equal(
    decodeURIComponent(address.slice(address.indexOf(",") + 1)),
    svg,
  );
});
