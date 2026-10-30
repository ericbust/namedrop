import { readFileSync } from "node:fs";

const css = readFileSync(
  process.argv[2] ?? new URL("mockup/tokens.css", import.meta.url),
  "utf8",
);

function block(selector) {
  const start = css.indexOf(selector);
  if (start < 0) throw new Error(`missing ${selector}`);
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    if (css[i] === "{") depth++;
    if (css[i] === "}" && --depth === 0) return css.slice(open + 1, i);
  }
  throw new Error(`unclosed ${selector}`);
}
function tokens(text) {
  return Object.fromEntries(
    [...text.matchAll(/--([\w-]+):\s*(oklch\([^)]*\)|#[0-9a-f]{3,8})/gi)].map(
      (m) => [m[1], m[2]],
    ),
  );
}
function oklchToLinear(l, c, h) {
  const a = c * Math.cos((h * Math.PI) / 180);
  const b = c * Math.sin((h * Math.PI) / 180);
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
    -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
    -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
  ].map((v) => Math.min(1, Math.max(0, v)));
}
function linear(value) {
  if (value.startsWith("#")) {
    const hex =
      value.length === 4
        ? [...value.slice(1)].map((x) => x + x).join("")
        : value.slice(1, 7);
    return [0, 2, 4].map((i) => {
      const v = Number.parseInt(hex.slice(i, i + 2), 16) / 255;
      return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    });
  }
  const [l, c, h] = value
    .match(/oklch\(([^)]*)\)/)[1]
    .split(/\s+/)
    .map((v) =>
      v.endsWith("%") ? Number.parseFloat(v) / 100 : Number.parseFloat(v),
    );
  return oklchToLinear(l, c, h);
}
const luminance = (v) => {
  const [r, g, b] = linear(v);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const ratio = (a, b) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};

const pairs = [
  ["text", "bg", 4.5],
  ["text", "bg-sidebar", 4.5],
  ["text", "bg-raised", 4.5],
  ["text-muted", "bg", 4.5],
  ["text-muted", "bg-sidebar", 4.5],
  ["text-muted", "bg-raised", 4.5],
  ["text-muted", "bg-hover", 4.5],
  ["accent-text", "bg", 4.5],
  ["accent-text", "bg-selected", 4.5],
  ["text", "bg-selected", 4.5],
  ["text-muted", "bg-selected", 4.5],
  ["held-text", "bg-selected", 4.5],
  ["failed-text", "bg-selected", 4.5],
  ["attention-text", "attention-bg", 4.5],
  ["text", "attention-bg", 4.5],
  ["text-muted", "attention-bg", 4.5],
  ["failed-text", "attention-bg", 4.5],
  ["text-muted", "warning-bg", 4.5],
  ["held-text", "bg", 4.5],
  ["failed-text", "bg", 4.5],
  ["warning-text", "warning-bg", 4.5],
  ["on-accent", "accent", 4.5],
  ["working", "bg", 3],
  ["waiting", "bg", 3],
  ["blocked", "bg", 3],
  ["idle", "bg", 3],
  ["offline", "bg", 3],
  ["working", "bg-sidebar", 3],
  ["working", "bg-selected", 3],
  ["blocked", "bg-sidebar", 3],
  ["focus", "bg", 3],
  ["focus", "bg-sidebar", 3],
  ["line-strong", "bg", 1.4],
];
let failed = 0;
for (const [name, selector] of [
  ["light", ":root {"],
  ["dark", ':root[data-theme="dark"]'],
]) {
  const t = {
    ...tokens(block(":root {")),
    ...(name === "dark" ? tokens(block(selector)) : {}),
  };
  console.log(`\n${name}`);
  for (const [fg, bg, min] of pairs) {
    if (!t[fg] || !t[bg]) {
      console.log(`  missing ${fg} or ${bg}`);
      failed++;
      continue;
    }
    const r = ratio(t[fg], t[bg]);
    const ok = r >= min;
    if (!ok) failed++;
    console.log(
      `  ${ok ? "ok  " : "FAIL"} ${fg.padEnd(15)} on ${bg.padEnd(13)} ${r.toFixed(2)} (min ${min})`,
    );
  }
}
process.exitCode = failed ? 1 : 0;
