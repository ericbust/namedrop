import { type CSSProperties, useEffect, useRef, useState } from "react";
import {
  centerMinimum,
  clampWidth,
  type PanelWidths,
  panelMinimum,
  panelStorageKey,
  panelWidths,
  readPanelWidths,
} from "../panelWidths.ts";

export function usePanelWidths(open: boolean) {
  const [viewport, setViewport] = useState(window.innerWidth);
  const [preferred, setPreferred] = useState<Partial<PanelWidths>>(() => {
    try {
      return readPanelWidths(localStorage.getItem(panelStorageKey));
    } catch {
      return {};
    }
  });
  useEffect(() => {
    const update = () => setViewport(window.innerWidth);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  useEffect(() => {
    try {
      localStorage.setItem(panelStorageKey, JSON.stringify(preferred));
    } catch {
      /* Resizing also works when storage is unavailable. */
    }
  }, [preferred]);
  const widths = panelWidths(preferred, viewport, open);
  const style: CSSProperties & {
    "--sidebar-width": string;
    "--slot-width": string;
  } = {
    "--sidebar-width": `${widths.left}px`,
    "--slot-width": `${widths.right}px`,
  };
  const maximum = {
    left: Math.max(
      panelMinimum.left,
      Math.min(360, viewport - centerMinimum - (open ? widths.right : 0)),
    ),
    right: Math.max(
      panelMinimum.right,
      Math.min(640, viewport - widths.left - centerMinimum),
    ),
  };
  return {
    style,
    widths,
    maximum,
    setWidth: (side: keyof PanelWidths, value: number) =>
      setPreferred((previous) => ({
        ...previous,
        [side]: clampWidth(value, panelMinimum[side], maximum[side]),
      })),
  };
}

type Props = {
  side: keyof PanelWidths;
  value: number;
  maximum: number;
  controls: string;
  onResize: (value: number) => void;
};
export function PanelResize({
  side,
  value,
  maximum,
  controls,
  onResize,
}: Props) {
  const drag = useRef<{ pointerId: number; x: number; width: number } | null>(
    null,
  );
  const [dragging, setDragging] = useState(false);
  useEffect(() => {
    if (!dragging) return;
    document.body.classList.add("resizing-panels");
    const stopAtNarrowSize = () => {
      if (window.innerWidth <= 860) {
        drag.current = null;
        setDragging(false);
      }
    };
    window.addEventListener("resize", stopAtNarrowSize);
    return () => {
      document.body.classList.remove("resizing-panels");
      window.removeEventListener("resize", stopAtNarrowSize);
    };
  }, [dragging]);
  const finish = () => {
    drag.current = null;
    setDragging(false);
  };
  return (
    <hr
      className={`panel-resize panel-resize-${side}${dragging ? " dragging" : ""}`}
      tabIndex={0}
      aria-label={
        side === "left"
          ? "Resize Channels sidebar"
          : "Resize conversation sidebar"
      }
      aria-orientation="vertical"
      aria-controls={controls}
      aria-valuemin={panelMinimum[side]}
      aria-valuemax={maximum}
      aria-valuenow={value}
      aria-valuetext={`${value} pixels`}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary) return;
        event.preventDefault();
        event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        drag.current = {
          pointerId: event.pointerId,
          x: event.clientX,
          width: value,
        };
        setDragging(true);
      }}
      onPointerMove={(event) => {
        const start = drag.current;
        if (start?.pointerId === event.pointerId)
          onResize(
            start.width +
              (event.clientX - start.x) * (side === "left" ? 1 : -1),
          );
      }}
      onPointerUp={(event) => {
        if (drag.current?.pointerId !== event.pointerId) return;
        finish();
        event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onPointerCancel={() => {
        if (drag.current) onResize(drag.current.width);
        finish();
      }}
      onLostPointerCapture={finish}
      onKeyDown={(event) => {
        let next: number;
        switch (event.key) {
          case "ArrowLeft":
            next =
              value + (side === "left" ? -1 : 1) * (event.shiftKey ? 40 : 10);
            break;
          case "ArrowRight":
            next =
              value + (side === "left" ? 1 : -1) * (event.shiftKey ? 40 : 10);
            break;
          case "Home":
            next = panelMinimum[side];
            break;
          case "End":
            next = maximum;
            break;
          default:
            return;
        }
        event.preventDefault();
        onResize(next);
      }}
    />
  );
}
