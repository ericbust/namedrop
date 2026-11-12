import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { diagramCopy } from "../copy.ts";
import type { DiagramRefusal, Drawn, Palette } from "../diagram.ts";
import { Icon } from "./Icons.tsx";

// Used when a browser cannot read one of the page's colors.
const LIGHT: Palette = {
  page: "#fdfdff",
  raised: "#f3f4f7",
  hover: "#ebecf1",
  selected: "#e0e5fe",
  line: "#e2e3e7",
  lineStrong: "#caccd1",
  text: "#1b1e27",
  muted: "#5d606b",
  font: "system-ui, sans-serif",
};
const TOKENS = {
  page: "--bg",
  raised: "--bg-raised",
  hover: "--bg-hover",
  selected: "--bg-selected",
  line: "--line",
  lineStrong: "--line-strong",
  text: "--text",
  muted: "--text-muted",
} as const;
const UNSET = "#010203";

let canvas: CanvasRenderingContext2D | null | undefined;
/** The page writes its colors as oklch; Mermaid's theme engine reads sRGB hex. */
function hex(color: string, fallback: string): string {
  if (canvas === undefined)
    canvas = document
      .createElement("canvas")
      .getContext("2d", { willReadFrequently: true });
  if (!canvas || color === "") return fallback;
  canvas.fillStyle = UNSET;
  canvas.fillStyle = color;
  if (canvas.fillStyle === UNSET) return fallback;
  canvas.fillRect(0, 0, 1, 1);
  const [red = 0, green = 0, blue = 0] = canvas.getImageData(0, 0, 1, 1).data;
  return `#${[red, green, blue].map((part) => part.toString(16).padStart(2, "0")).join("")}`;
}

let palette: Palette | null = null;
function readPalette(): Palette {
  if (palette) return palette;
  const page = getComputedStyle(document.documentElement);
  const colors = Object.fromEntries(
    Object.entries(TOKENS).map(([name, token]) => [
      name,
      hex(
        page.getPropertyValue(token).trim(),
        LIGHT[name as keyof typeof TOKENS],
      ),
    ]),
  ) as Record<keyof typeof TOKENS, string>;
  const font = page.getPropertyValue("--font-ui").replace(/\s+/g, " ").trim();
  palette = { ...colors, font: font || LIGHT.font };
  return palette;
}
/** The page's look changes with the system setting or its `data-theme`. */
function watchPalette(changed: () => void): () => void {
  const refresh = () => {
    palette = null;
    changed();
  };
  const scheme = matchMedia("(prefers-color-scheme: dark)");
  scheme.addEventListener("change", refresh);
  const theme = new MutationObserver(refresh);
  theme.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
  return () => {
    scheme.removeEventListener("change", refresh);
    theme.disconnect();
  };
}

// Drawings already made, so a message shown again has its picture at once
// and does not flash its source first.
const KEPT = 64;
const made = new Map<string, Drawn>();
const madeKey = (source: string, look: Palette) =>
  `${Object.values(look).join("|")}\n${source}`;
function keep(source: string, look: Palette, drawn: Drawn) {
  made.set(madeKey(source, look), drawn);
  if (made.size > KEPT) {
    const [oldest] = made.keys();
    if (oldest !== undefined) made.delete(oldest);
  }
}

let drawer: Promise<typeof import("../diagramDraw.ts")> | null = null;
function draw(source: string, look: Palette): Promise<Drawn | null> {
  // The built page loads the separately built drawing code by its address.
  drawer ??= import.meta.env.DEV
    ? import("../diagramDraw.ts")
    : import(/* @vite-ignore */ __DIAGRAM_ENTRY__);
  return drawer.then(
    (module) => module.draw(source, look),
    () => {
      // A failed load, as during an update, is tried again by the next diagram.
      drawer = null;
      return null;
    },
  );
}

/** Whether any of the element is on screen, so only diagrams in view are drawn. */
function useOnScreen(element: { readonly current: Element | null }): boolean {
  const [onScreen, setOnScreen] = useState(false);
  useEffect(() => {
    const target = element.current;
    if (!target) return;
    // The last entry is the newest state when several arrive together.
    const watch = new IntersectionObserver((entries) =>
      setOnScreen(entries.at(-1)?.isIntersecting ?? false),
    );
    watch.observe(target);
    return () => watch.disconnect();
  }, [element]);
  return onScreen;
}

/**
 * A fenced `mermaid` block. Until it is drawn, and when it cannot be, the
 * reader sees its source, as for any other code block. `refusal` says the
 * block is over a limit and is not drawn at all.
 */
export function Diagram({
  source,
  refusal = null,
}: {
  source: string;
  refusal?: DiagramRefusal | null;
}) {
  const look = useSyncExternalStore(watchPalette, readPalette);
  const key = madeKey(source, look);
  // The picture on show, and the look it was drawn for. After the page's
  // look changes, a diagram keeps its old picture until it is in view again.
  const [shown, setShown] = useState<{
    key: string;
    picture: Drawn | "failed";
  } | null>(() => {
    // A refused block is never a picture, even if the same source was drawn elsewhere.
    const kept = refusal === null ? made.get(key) : undefined;
    return kept ? { key, picture: kept } : null;
  });
  const [showSource, setShowSource] = useState(false);
  const [copied, setCopied] = useState(false);
  const frame = useRef<HTMLElement>(null);
  const full = useRef<HTMLDialogElement>(null);
  const onScreen = useOnScreen(frame);
  const due = refusal === null && onScreen && shown?.key !== key;
  useEffect(() => {
    if (!due) return;
    let current = true;
    void draw(source, look).then((result) => {
      if (result) keep(source, look, result);
      if (current) setShown({ key, picture: result ?? "failed" });
    });
    return () => {
      current = false;
    };
  }, [due, source, look, key]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  const picture = shown?.picture ?? null;
  const image =
    refusal === null && typeof picture === "object" ? picture : null;
  const copy = () => {
    void navigator.clipboard?.writeText(source).then(
      () => setCopied(true),
      () => {},
    );
  };
  return (
    <figure
      ref={frame}
      className="diagram"
      data-state={image ? "drawn" : (refusal ?? picture ?? "drawing")}
    >
      {image && (
        <div className="diagram-box">
          <button
            type="button"
            className="diagram-view"
            aria-label={diagramCopy.open}
            title={diagramCopy.open}
            onClick={() => full.current?.showModal()}
          >
            <img
              src={image.src}
              width={image.width}
              height={image.height}
              alt={diagramCopy.alt}
              draggable={false}
            />
          </button>
        </div>
      )}
      {(!image || showSource) && (
        <pre>
          <code>{source}</code>
        </pre>
      )}
      <figcaption>
        {refusal !== null ? (
          <span className="diagram-note">{diagramCopy.refused[refusal]}</span>
        ) : (
          picture === "failed" && (
            <span className="diagram-note">{diagramCopy.failed}</span>
          )
        )}
        {image && (
          <button
            type="button"
            className="text-button"
            aria-expanded={showSource}
            onClick={() => setShowSource(!showSource)}
          >
            {showSource ? diagramCopy.hideSource : diagramCopy.showSource}
          </button>
        )}
        <button type="button" className="text-button" onClick={copy}>
          {copied ? diagramCopy.copied : diagramCopy.copy}
        </button>
      </figcaption>
      {image && (
        // biome-ignore lint/a11y/useKeyWithClickEvents: Escape and the close button close it from the keyboard.
        <dialog
          ref={full}
          className="diagram-full"
          aria-label={diagramCopy.alt}
          onClick={(event) => {
            // The dialog element itself is the dimmed area around the diagram.
            if (event.target === event.currentTarget)
              event.currentTarget.close();
          }}
        >
          <div className="diagram-full-pane">
            <button
              type="button"
              className="diagram-close"
              aria-label={diagramCopy.close}
              onClick={() => full.current?.close()}
            >
              <Icon name="x" />
            </button>
            <div className="diagram-full-scroll">
              <img
                src={image.src}
                width={image.width}
                height={image.height}
                alt={diagramCopy.alt}
                draggable={false}
              />
            </div>
          </div>
        </dialog>
      )}
    </figure>
  );
}
