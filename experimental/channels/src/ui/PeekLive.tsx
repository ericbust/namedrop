import { Terminal } from "@xterm/xterm";
import { useEffect, useRef } from "react";
import { peekCopy } from "../copy.ts";
import { type LiveState, observePeek } from "../livePeek.ts";
import type { PeekTarget } from "../model.ts";
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
    let interrupted = false;
    let terminal: Terminal | null = null;
    const pending = new Set<() => void>();
    let following = true;
    let replacing = false;
    let position = { top: 0, left: 0 };
    let columns = 0;
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
      const anchor = changed && !following ? readingAnchor() : null;
      if (changed) {
        replacing = true;
        const fragment = document.createDocumentFragment();
        for (const line of lines) {
          const span = document.createElement("span");
          span.dataset.rowId = String(line.id);
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
      const current = terminal;
      if (!current || !columns) return;
      const style = getComputedStyle(scroller);
      const available =
        scroller.clientWidth -
        Number.parseFloat(style.paddingLeft) -
        Number.parseFloat(style.paddingRight);
      if (available <= 0) return;
      current.options.fontSize = baseFont;
      const width =
        current.element?.querySelector(".xterm-screen")?.getBoundingClientRect()
          .width ?? 0;
      if (width <= 0) return;
      const fitted = Math.max(10, Math.min(20, (baseFont * available) / width));
      current.options.fontSize = fitted;
      historyNode.style.fontSize = `${fitted}px`;
      restorePosition();
    }
    function latestTop() {
      const current = terminal;
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
      if (replacing) return;
      following = scroller.scrollTop >= latestTop() - 1;
      position = { top: scroller.scrollTop, left: scroller.scrollLeft };
    }
    function restorePosition() {
      if (controller.signal.aborted) return;
      // Repeated assignments cancel Chromium's native keyboard scroll animation.
      // A reader's position needs restoration only when replacing the grid.
      if (following || replacing) {
        const top = following ? latestTop() : position.top;
        if (Math.abs(scroller.scrollTop - top) > 1) scroller.scrollTop = top;
      }
      if (replacing && scroller.scrollLeft !== position.left)
        scroller.scrollLeft = position.left;
      position = { top: scroller.scrollTop, left: scroller.scrollLeft };
    }
    function followIntent(event: KeyboardEvent) {
      if (event.key === "End" && (event.ctrlKey || event.metaKey))
        following = true;
    }
    function localWheel(event: WheelEvent) {
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
    const resize = new ResizeObserver(() => {
      if (!replacing) fit();
    });
    resize.observe(scroller);
    function dispose() {
      for (const cancel of pending) cancel();
      pending.clear();
      terminal?.dispose();
      terminal = null;
      element?.replaceChildren();
    }
    function create(cols: number, rows: number) {
      dispose();
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
      next.parser.registerCsiHandler({ final: "J" }, () => {
        interrupted = true;
        return false;
      });
      next.parser.registerEscHandler({ final: "c" }, () => {
        interrupted = true;
        return false;
      });
      next.parser.registerCsiHandler({ final: "h", prefix: "?" }, (params) => {
        if ([47, 1047, 1049].some((mode) => params.includes(mode)))
          interrupted = true;
        return false;
      });
      next.parser.registerCsiHandler({ final: "l", prefix: "?" }, (params) => {
        if ([47, 1047, 1049].some((mode) => params.includes(mode)))
          interrupted = true;
        return false;
      });
      next.onRender(restorePosition);
      terminal = next;
      next.open(element);
      columns = cols;
      next.textarea?.setAttribute("tabindex", "-1");
      // xterm initializes its cursor on local focus. Preserve browser focus;
      // there is no remote focus or input transport.
      const focused = document.activeElement;
      next.focus();
      if (focused instanceof HTMLElement && focused.isConnected)
        focused.focus({ preventScroll: true });
      else next.blur();
      fit();
      return next;
    }
    async function write(current: Terminal, bytes: Uint8Array) {
      for (let start = 0; start < bytes.length; start += 64 * 1024) {
        if (controller.signal.aborted || terminal !== current)
          throw new Error("Disposed terminal");
        await new Promise<void>((resolve, reject) => {
          const cancel = () => {
            pending.delete(cancel);
            reject(new Error("Disposed terminal"));
          };
          pending.add(cancel);
          try {
            current.write(bytes.subarray(start, start + 64 * 1024), () => {
              pending.delete(cancel);
              if (controller.signal.aborted || terminal !== current)
                reject(new Error("Disposed terminal"));
              else resolve();
            });
          } catch {
            cancel();
          }
        });
      }
    }
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
        interrupted = false;
        let current = terminal;
        if (update.kind === "reset") {
          if (!current)
            current = create(update.viewport.cols, update.viewport.rows);
          else {
            current.reset();
            current.resize(update.viewport.cols, update.viewport.rows);
            columns = update.viewport.cols;
            fit();
          }
        }
        if (!current) throw new Error("Terminal not initialized");
        await write(current, update.bytes);
        if (update.kind === "reset") {
          current.refresh(0, current.rows - 1);
          validated = true;
          watchState.current = watchReduce(watchState.current, {
            kind: "baseline",
            continuity: update.continuity,
            at: Date.now(),
            frame: sample(current),
          });
        } else {
          watchState.current = watchReduce(watchState.current, {
            kind: "frame",
            frame: sample(current),
            interrupted,
          });
        }
        snapshot();
        restorePosition();
        replacing = false;
      },
      onState(state) {
        if (controller.signal.aborted) return;
        if (
          state.kind === "session-ended" ||
          (state.kind === "closed" && state.reason !== "ended")
        ) {
          validated = false;
          watchState.current = emptyWatch();
          snapshot();
          dispose();
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
      dispose();
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
