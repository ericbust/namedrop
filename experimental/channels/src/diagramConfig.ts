// Part of the separately built drawing code. Whatever this imports is built
// into that code too, and a change there renames Mermaid's committed files,
// so it imports types only.
import type { Palette } from "./diagram.ts";

/** Light text means a dark page. */
export function isDark(palette: Palette): boolean {
  const channel = (at: number) =>
    Number.parseInt(palette.text.slice(at, at + 2), 16);
  return 0.299 * channel(1) + 0.587 * channel(3) + 0.114 * channel(5) > 140;
}

/** Settings a diagram's own `%%{init}%%` or front matter cannot change. */
const LOCKED = [
  "secure",
  "securityLevel",
  "startOnLoad",
  "maxTextSize",
  "suppressErrorRendering",
  "maxEdges",
  "htmlLabels",
  "layout",
  "theme",
  "themeVariables",
  "themeCSS",
  "darkMode",
  "fontFamily",
  "altFontFamily",
];

export function diagramConfig(palette: Palette) {
  return {
    startOnLoad: false,
    // No click handlers, and tags in labels are text.
    securityLevel: "strict",
    // Labels are SVG text. HTML labels would need foreignObject, which the
    // picture drops, and which an image cannot show anyway.
    htmlLabels: false,
    // A diagram that fails to parse is shown as its source, not as an error drawing.
    suppressErrorRendering: true,
    secure: LOCKED,
    // Mermaid 12 lays out with ELK unless told otherwise. ELK is 1.5 MB to
    // load and slow on a large graph, so layout is dagre and a diagram cannot
    // ask for another.
    layout: "dagre",
    theme: "base",
    darkMode: isDark(palette),
    fontFamily: palette.font,
    themeVariables: {
      darkMode: isDark(palette),
      fontFamily: palette.font,
      fontSize: "14px",
      // Mermaid's grey shadow reads as a glow on a dark page.
      dropShadow: "none",
      background: palette.raised,
      primaryColor: palette.page,
      primaryTextColor: palette.text,
      primaryBorderColor: palette.lineStrong,
      secondaryColor: palette.selected,
      secondaryTextColor: palette.text,
      secondaryBorderColor: palette.lineStrong,
      tertiaryColor: palette.hover,
      tertiaryTextColor: palette.text,
      tertiaryBorderColor: palette.line,
      lineColor: palette.muted,
      textColor: palette.text,
      titleColor: palette.text,
      edgeLabelBackground: palette.raised,
      noteBkgColor: palette.selected,
      noteTextColor: palette.text,
      noteBorderColor: palette.lineStrong,
    },
  } as const;
}

/** One cache key per source and look, so a theme change draws again. */
export function drawingKey(source: string, palette: Palette): string {
  return `${Object.values(palette).join("|")}\n${source}`;
}

/** An SVG document as an image address the page's policy allows. */
export function imageAddress(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}
