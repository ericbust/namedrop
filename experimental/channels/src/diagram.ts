import type { Block } from "./markdown.ts";

type Code = Extract<Block, { kind: "code" }>;

/** A fenced block tagged `mermaid`, whatever its case. */
export function isDiagram(block: Code): boolean {
  return block.lang.toLowerCase() === "mermaid";
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
