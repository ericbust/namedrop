// Loaded only when a message holds a diagram, so Mermaid stays out of the
// page's first load.
import DOMPurify from "dompurify";
import mermaid from "mermaid";
import type { Drawn, Palette } from "./diagram.ts";
import { diagramConfig, drawingKey, imageAddress } from "./diagramConfig.ts";

const KEPT = 64;
const drawings = new Map<string, Promise<Drawn | null>>();
let turn: Promise<unknown> = Promise.resolve();
let stage: HTMLElement | null = null;
let drawn = 0;

/** Draws `source`, or resolves to `null` when Mermaid cannot. */
export function draw(source: string, palette: Palette): Promise<Drawn | null> {
  const key = drawingKey(source, palette);
  const kept = drawings.get(key);
  if (kept) return kept;
  // Mermaid keeps one configuration, so drawings take turns.
  const drawing = turn.then(() => drawNow(source, palette));
  turn = drawing;
  drawings.set(key, drawing);
  if (drawings.size > KEPT) {
    const [oldest] = drawings.keys();
    if (oldest !== undefined) drawings.delete(oldest);
  }
  return drawing;
}

async function drawNow(
  source: string,
  palette: Palette,
): Promise<Drawn | null> {
  // Mermaid measures text in the page, so it needs an element that is laid out.
  stage ??= document.body.appendChild(
    Object.assign(document.createElement("div"), {
      className: "diagram-stage",
      ariaHidden: "true",
    }),
  );
  const id = `diagram-${++drawn}`;
  try {
    mermaid.initialize(diagramConfig(palette));
    const { svg } = await mermaid.render(id, source, stage);
    return picture(svg);
  } catch {
    return null;
  } finally {
    stage.replaceChildren();
    for (const leftover of [id, `d${id}`])
      document.getElementById(leftover)?.remove();
  }
}

/**
 * The drawing as a standalone SVG image. An image runs no script and loads
 * nothing, whatever it holds; the cleaning is the second barrier.
 */
function picture(svg: string): Drawn | null {
  const clean = DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["foreignObject", "a", "image", "script"],
    RETURN_DOM_FRAGMENT: true,
  });
  const root = clean.querySelector("svg");
  if (!root) return null;
  const box = root.viewBox.baseVal;
  const width = Math.ceil(box.width);
  const height = Math.ceil(box.height);
  if (!(width > 0 && height > 0)) return null;
  // Mermaid sizes its drawing to a container; an image needs its own size.
  root.setAttribute("width", String(width));
  root.setAttribute("height", String(height));
  root.removeAttribute("style");
  return {
    src: imageAddress(new XMLSerializer().serializeToString(root)),
    width,
    height,
  };
}
