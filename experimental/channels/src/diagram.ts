import type { Block } from "./markdown.ts";

type Code = Extract<Block, { kind: "code" }>;

/** A fenced block tagged `mermaid`, whatever its case. */
export function isDiagram(block: Code): boolean {
  return block.lang.toLowerCase() === "mermaid";
}

/** Past these, a block is shown as its source: Mermaid lays out on the page's
 * own thread, so one heavy diagram stalls the tab and many stall it for long. */
export const MAX_DIAGRAM_SOURCE = 10_000;
export const MAX_DIAGRAMS_PER_MESSAGE = 8;
export type DiagramRefusal = "size" | "count";

/** A drawing wider than its column is shown smaller to fit, down to this share
 * of its drawn size, where its 14 px labels are about 10 px. Below that they
 * are too small to read, so the drawing keeps this size and its box scrolls
 * sideways. */
export const MIN_DIAGRAM_SCALE = 0.7;

/** Counts the diagrams one message draws, in the order they appear. */
export function diagramBudget(): (source: string) => DiagramRefusal | null {
  let drawn = 0;
  return (source) => {
    if (source.length > MAX_DIAGRAM_SOURCE) return "size";
    if (drawn === MAX_DIAGRAMS_PER_MESSAGE) return "count";
    drawn += 1;
    return null;
  };
}

/** The page's colors as sRGB hex, which is all Mermaid's theme engine reads. */
export type Palette = {
  readonly page: string;
  readonly raised: string;
  readonly hover: string;
  readonly selected: string;
  readonly line: string;
  readonly lineStrong: string;
  readonly text: string;
  readonly muted: string;
  readonly font: string;
};

/** A drawn diagram: a picture the browser shows as an image, and its size. */
export type Drawn = {
  readonly src: string;
  readonly width: number;
  readonly height: number;
};
