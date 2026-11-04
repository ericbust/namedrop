import { Terminal } from "@xterm/xterm";
import { useEffect, useRef } from "react";
import { peekCopy } from "../copy.ts";
import { type LiveState, observePeek } from "../livePeek.ts";
import type { PeekTarget } from "../model.ts";
import { PeekPresentation } from "../peekPresentation.ts";
import {
  emptyWatch,
  type WatchFrame,
  type WatchHistory,
  type WatchRow,
  watchReduce,
  watchRows,
} from "../watchHistory.ts";
import "@xterm/xterm/css/xterm.css";

export function PeekLive({
  target,
  onState,
  retainedHistory,
  onHistory,
}: {
  target: PeekTarget;
  onState(state: LiveState): void;
  retainedHistory: WatchHistory;
  onHistory(state: WatchHistory): void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const history = useRef<HTMLPreElement>(null);
  const historyRegion = useRef<HTMLDivElement>(null);
  const viewport = useRef<HTMLElement>(null);
  const watching = useRef<HTMLParagraphElement>(null);
  const watchState = useRef(retainedHistory);
  useEffect(() => {
    const mounted = host.current;
    const scrolling = viewport.current;
    const historyElement = history.current;
    const historyBlock = historyRegion.current;
    if (!mounted || !scrolling || !historyElement || !historyBlock) return;
    const element = mounted;
    const scroller = scrolling;
    const historyNode = historyElement;
    const historyContainer = historyBlock;
    const controller = new AbortController();
    let validated = false;
    let presentation: PeekPresentation;
    let navigation: "following" | "reading" | "returning" = "following";
    let replacing = false;
    let position = { top: 0, left: 0 };
    let columns = 0;
    let fittedSize = { width: 0, height: 0 };
    let renderedRows: readonly WatchRow[] = [];
    const baseFont = Number.parseFloat(getComputedStyle(element).fontSize);
    function readingAnchor() {
      const top = scroller.getBoundingClientRect().top;
      const lines = historyNode.children;
      for (let i = 0; i < lines.length; i++) {
        const line = lines[i];
        if (!(line instanceof HTMLElement)) continue;
        const rect = line.getBoundingClientRect();
        if (rect.bottom > top) {
          return {
            id: line.dataset.rowId,
            offset: rect.top - top,
          };
        }
      }
      return null;
    }
    function snapshot() {
      const lines = validated ? watchRows(watchState.current) : [];
      const changed =
        lines.length !== renderedRows.length ||
        lines.some((row, i) => row !== renderedRows[i]);
      const anchor =
        changed && navigation === "reading" ? readingAnchor() : null;
      if (changed) {
        replacing = true;
        const fragment = document.createDocumentFragment();
        for (const line of lines) {
          const span = document.createElement("span");
          span.dataset.rowId = String(line.id);
          span.dataset.watchKind = line.kind;
          span.textContent = line.text || " ";
          fragment.append(span);
        }
        historyNode.replaceChildren(fragment);
        renderedRows = lines;
        historyContainer.hidden = lines.length === 0;
      }
      const marker = watching.current;
      const since = validated ? watchState.current.startedAt : null;
      if (marker) {
        marker.hidden = since === null;
        marker.textContent =
          since === null
            ? ""
            : `Watching since ${new Date(since).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
      }
      if (anchor) {
        const line = Array.from(historyNode.children).find(
          (node) =>
            node instanceof HTMLElement && node.dataset.rowId === anchor.id,
        );
        if (line instanceof HTMLElement) {
          position.top +=
            line.getBoundingClientRect().top -
            scroller.getBoundingClientRect().top -
            anchor.offset;
        } else {
          position.top = 0;
        }
      }
      onHistory(watchState.current);
    }
    function sample(current: Terminal): WatchFrame {
      const buffer = current.buffer.active;
      return {
        cols: current.cols,
        buffer: buffer.type,
        rows: Array.from(
          { length: current.rows },
          (_, y) =>
            buffer.getLine(buffer.baseY + y)?.translateToString(true) ?? "",
        ),
      };
    }
    function fit() {
      const current = presentation.visible;
      if (!current || !columns) return;
      const style = getComputedStyle(scroller);
      const available =
        scroller.clientWidth -
        Number.parseFloat(style.paddingLeft) -
        Number.parseFloat(style.paddingRight);
      if (available <= 0) return;
      const width =
        current.element?.querySelector(".xterm-screen")?.getBoundingClientRect()
          .width ?? 0;
      if (width <= 0) return;
      const fitted = Math.max(
        10,
        Math.min(20, ((current.options.fontSize ?? baseFont) * available) / width),
      );
      void presentation.setFontSize(fitted);
      restorePosition();
      fittedSize = { width: scroller.clientWidth, height: scroller.clientHeight };
    }
    function latestTop() {
      const current = presentation.visible;
      const screen = current?.element;
      if (!current || !screen) return 0;
      const buffer = current.buffer.active;
      let row = buffer.cursorY;
      for (let y = current.rows - 1; y > row; y--) {
        if (
          buffer
            .getLine(buffer.baseY + y)
            ?.translateToString(true)
            .trim()
        ) {
          row = y;
          break;
        }
      }
      // The projected grid can end in blank rows. Keep the lowest retained text
      // or cursor row in view rather than following that empty physical tail.
      const grid = screen.getBoundingClientRect();
      const start =
        grid.top - scroller.getBoundingClientRect().top + scroller.scrollTop;
      const padding = Number.parseFloat(
        getComputedStyle(scroller).paddingBottom,
      );
      return Math.max(
        0,
        Math.min(
          scroller.scrollHeight - scroller.clientHeight,
          start +
            ((row + 1) * grid.height) / current.rows +
            padding -
            scroller.clientHeight,
        ),
      );
    }
    function rememberPosition() {
      if (
        replacing ||
        scroller.clientWidth !== fittedSize.width ||
        scroller.clientHeight !== fittedSize.height
      )
        return;
      const latest = scroller.scrollTop >= latestTop() - 1;
      if (navigation !== "returning" || latest)
        navigation = latest ? "following" : "reading";
      position = { top: scroller.scrollTop, left: scroller.scrollLeft };
    }
    function restorePosition() {
      if (controller.signal.aborted) return;
      // Repeated assignments cancel Chromium's native keyboard scroll animation.
      // A reader's position needs restoration only when replacing the grid.
      if (navigation !== "reading" || replacing) {
        const top = navigation !== "reading" ? latestTop() : position.top;
        if (Math.abs(scroller.scrollTop - top) > 1) scroller.scrollTop = top;
      }
      if (replacing && scroller.scrollLeft !== position.left)
        scroller.scrollLeft = position.left;
      position = { top: scroller.scrollTop, left: scroller.scrollLeft };
    }
    function followIntent(event: KeyboardEvent) {
      if (event.key === "End" && (event.ctrlKey || event.metaKey))
        navigation = "returning";
      else if (
        [
          "ArrowUp",
          "ArrowDown",
          "PageUp",
          "PageDown",
          "Home",
          "End",
          " ",
        ].includes(event.key)
      )
        interruptReturn();
    }
    function interruptReturn() {
      if (navigation === "returning") navigation = "reading";
    }
    function localWheel(event: WheelEvent) {
      interruptReturn();
      // Keep the browser default, but bypass xterm's wheel-to-input and mouse
      // tracking handlers even when the projected TUI enables those modes.
      event.stopPropagation();
    }
    scroller.addEventListener("wheel", localWheel, {
      capture: true,
      passive: true,
    });
    scroller.addEventListener("scroll", rememberPosition);
    scroller.addEventListener("keydown", followIntent);
    scroller.addEventListener("pointerdown", interruptReturn);
    const resize = new ResizeObserver(() => {
      if (!replacing) fit();
    });
    resize.observe(scroller);
    function create(cols: number, rows: number) {
      const style = getComputedStyle(element);
      const next = new Terminal({
        cols,
        rows,
        disableStdin: true,
        scrollback: 0,
        convertEol: false,
        cursorBlink: false,
        cursorInactiveStyle: "block",
        altClickMovesCursor: false,
        fontFamily: style.fontFamily,
        fontSize: Number.parseFloat(style.fontSize),
        lineHeight: 1.5,
        windowOptions: {},
        logLevel: "off",
        allowProposedApi: false,
        linkHandler: { activate() {}, allowNonHttpProtocols: false },
      });
      for (const id of [0, 1, 2, 8, 52])
        next.parser.registerOscHandler(id, () => true);
      next.parser.registerCsiHandler({ final: "t" }, () => true);
      next.onRender(() => {
        if (presentation.visible === next) restorePosition();
      });
      columns = cols;
      return next;
    }
    presentation = new PeekPresentation(element, create, (paint) => {
      historyNode.style.fontSize = `${paint.terminal.options.fontSize}px`;
      if (paint.history) {
        if (paint.baseline !== null) {
          validated = true;
          watchState.current = watchReduce(watchState.current, {
            kind: "baseline",
            continuity: paint.baseline,
            at: Date.now(),
            frame: sample(paint.terminal),
          });
        } else {
          watchState.current = watchReduce(watchState.current, {
            kind: "frame",
            frame: sample(paint.terminal),
            interrupted: paint.interrupted,
          });
        }
        snapshot();
      }
      if (paint.baseline !== null) fit();
      restorePosition();
      replacing = false;
    });
    void observePeek({
      target,
      signal: controller.signal,
      retainedContinuity: watchState.current.continuity,
      async apply(update) {
        if (update.kind === "history") {
          replacing = true;
          watchState.current = watchReduce(watchState.current, {
            kind: "seed",
            text: update.text,
          });
          snapshot();
          restorePosition();
          replacing = false;
          return;
        }
        replacing = update.kind === "reset";
        await presentation.apply(update);
      },
      onState(state) {
        if (controller.signal.aborted) return;
        if (state.kind === "reconnecting") presentation.invalidate();
        if (
          state.kind === "session-ended" ||
          (state.kind === "closed" && state.reason !== "ended")
        ) {
          validated = false;
          watchState.current = emptyWatch();
          snapshot();
          presentation.dispose();
        }
        onState(state);
      },
    });
    return () => {
      controller.abort();
      watchState.current = watchReduce(watchState.current, { kind: "detach" });
      onHistory(watchState.current);
      resize.disconnect();
      scroller.removeEventListener("wheel", localWheel, true);
      scroller.removeEventListener("scroll", rememberPosition);
      scroller.removeEventListener("keydown", followIntent);
      scroller.removeEventListener("pointerdown", interruptReturn);
      presentation.dispose();
    };
  }, [target, onState, onHistory]);
  return (
    <section
      ref={viewport}
      className="peek-text peek-live"
      aria-label={peekCopy.liveRegion}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: The read-only viewport supports keyboard scrolling.
      tabIndex={0}
    >
      <p className="peek-watching" ref={watching} hidden />
      <div className="peek-history" ref={historyRegion} hidden>
        <p className="peek-region-label">Recent output</p>
        <pre ref={history} />
      </div>
      <p className="peek-region-label peek-screen-label">Live screen</p>
      <div className="peek-terminal" ref={host} />
    </section>
  );
}
