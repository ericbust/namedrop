import { Terminal } from "@xterm/xterm";
import { useEffect, useRef } from "react";
import { peekCopy } from "../copy.ts";
import { type LiveState, observePeek } from "../livePeek.ts";
import type { PeekTarget } from "../model.ts";
import "@xterm/xterm/css/xterm.css";

export function PeekLive({
  target,
  onState,
}: {
  target: PeekTarget;
  onState(state: LiveState): void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const viewport = useRef<HTMLElement>(null);
  useEffect(() => {
    const mounted = host.current;
    const scrolling = viewport.current;
    if (!mounted || !scrolling) return;
    const element = mounted;
    const scroller = scrolling;
    const controller = new AbortController();
    let terminal: Terminal | null = null;
    const pending = new Set<() => void>();
    let following = true;
    let replacing = false;
    let position = { top: 0, left: 0 };
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
    const resize = new ResizeObserver(() => {
      if (!replacing) restorePosition();
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
      next.onRender(restorePosition);
      terminal = next;
      next.open(element);
      next.textarea?.setAttribute("tabindex", "-1");
      // xterm initializes its cursor on local focus. Preserve browser focus;
      // there is no remote focus or input transport.
      const focused = document.activeElement;
      next.focus();
      if (focused instanceof HTMLElement && focused.isConnected)
        focused.focus({ preventScroll: true });
      else next.blur();
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
      async apply(update) {
        replacing = update.kind === "reset";
        const current =
          update.kind === "reset"
            ? create(update.viewport.cols, update.viewport.rows)
            : terminal;
        if (!current) throw new Error("Terminal not initialized");
        await write(current, update.bytes);
        restorePosition();
        replacing = false;
      },
      onState(state) {
        if (controller.signal.aborted) return;
        if (
          state.kind === "session-ended" ||
          (state.kind === "closed" && state.reason !== "ended")
        )
          dispose();
        onState(state);
      },
    });
    return () => {
      controller.abort();
      resize.disconnect();
      scroller.removeEventListener("wheel", localWheel, true);
      scroller.removeEventListener("scroll", rememberPosition);
      dispose();
    };
  }, [target, onState]);
  return (
    <section
      ref={viewport}
      className="peek-text peek-live"
      aria-label={peekCopy.liveRegion}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: The read-only viewport supports keyboard scrolling.
      tabIndex={0}
    >
      <div className="peek-terminal" ref={host} />
    </section>
  );
}
