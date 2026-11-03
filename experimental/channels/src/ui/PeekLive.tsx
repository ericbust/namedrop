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
  useEffect(() => {
    const mounted = host.current;
    if (!mounted) return;
    const element = mounted;
    const controller = new AbortController();
    let terminal: Terminal | null = null;
    const pending = new Set<() => void>();
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
        const current =
          update.kind === "reset"
            ? create(update.viewport.cols, update.viewport.rows)
            : terminal;
        if (!current) throw new Error("Terminal not initialized");
        await write(current, update.bytes);
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
      dispose();
    };
  }, [target, onState]);
  return (
    <section
      className="peek-text peek-live"
      aria-label={peekCopy.liveRegion}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: The read-only viewport supports keyboard scrolling.
      tabIndex={0}
    >
      <div className="peek-terminal" ref={host} />
    </section>
  );
}
