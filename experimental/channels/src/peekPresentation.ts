import type { Terminal } from "@xterm/xterm";
import type { LiveUpdate } from "./livePeek.ts";

type Paint = Exclude<LiveUpdate, { kind: "history" }>;
type Buffer = {
  terminal: Terminal;
  host: HTMLDivElement;
  interrupted: boolean;
  initialized: boolean;
};
type Hold = { deadline: number; timer: ReturnType<typeof setTimeout> | null };

export class PeekPresentation {
  private buffers: [Buffer, Buffer] | null = null;
  private queue = Promise.resolve();
  private generation = 0;
  private disposed = false;
  private pending = new Set<() => void>();
  private paints: Paint[] = [];
  private baseline: string | null = null;
  private hold: Hold | null = null;
  private skippedBlank = false;

  constructor(
    private readonly host: HTMLElement,
    private readonly create: (cols: number, rows: number) => Terminal,
    private readonly commit: (paint: {
      terminal: Terminal;
      baseline: string | null;
      interrupted: boolean;
      history: boolean;
    }) => void,
  ) {}

  get visible() {
    return this.buffers?.[0].terminal ?? null;
  }

  get terminals() {
    return this.buffers?.map((buffer) => buffer.terminal) ?? [];
  }

  invalidate() {
    this.generation++;
    if (this.hold?.timer) clearTimeout(this.hold.timer);
    this.hold = null;
  }

  apply(paint: Paint) {
    if (paint.kind === "reset") this.invalidate();
    const generation = this.generation;
    return this.enqueue(async () => {
      if (!this.active(generation)) return;
      if (!this.buffers) {
        if (paint.kind !== "reset") throw new Error("Terminal not initialized");
        this.buffers = [this.open(paint), this.open(paint)];
        this.show(this.buffers[0], true);
      }
      const staged = this.buffers[1];
      if (paint.kind === "reset") {
        this.paints = [];
        this.baseline = paint.continuity;
        this.skippedBlank = false;
      }
      this.paints.push(paint);
      await this.write(staged, paint);
      if (
        !this.active(generation) ||
        staged.terminal.modes.synchronizedOutputMode
      )
        return;
      const blank = this.blank(staged.terminal);
      if (blank && (!this.blank(this.buffers[0].terminal) || this.hold)) {
        this.skippedBlank = true;
        if (!this.hold) {
          const hold: Hold = { deadline: Date.now() + 120, timer: null };
          this.hold = hold;
          hold.timer = setTimeout(() => {
            hold.timer = null;
            void this.enqueue(async () => {
              if (
                !this.active(generation) ||
                this.hold !== hold ||
                staged.terminal.modes.synchronizedOutputMode
              )
                return;
              await this.publish(generation, false);
            }).catch(() => {});
          }, 120);
        } else if (Date.now() >= this.hold.deadline) {
          await this.publish(generation, false);
        }
        return;
      }
      await this.publish(generation, !blank || !this.skippedBlank);
    });
  }

  private enqueue(action: () => Promise<void>) {
    const next = this.queue.then(action);
    this.queue = next.catch(() => {});
    return next;
  }

  private active(generation: number) {
    return !this.disposed && generation === this.generation;
  }

  private open(paint: Extract<Paint, { kind: "reset" }>): Buffer {
    const host = document.createElement("div");
    host.className = "peek-buffer";
    this.host.append(host);
    const terminal = this.create(paint.viewport.cols, paint.viewport.rows);
    const buffer = { terminal, host, interrupted: false, initialized: false };
    const interrupt = () => {
      buffer.interrupted = true;
      return false;
    };
    terminal.parser.registerCsiHandler({ final: "J" }, interrupt);
    terminal.parser.registerEscHandler({ final: "c" }, interrupt);
    for (const final of ["h", "l"]) {
      terminal.parser.registerCsiHandler({ final, prefix: "?" }, (params) => {
        if ([47, 1047, 1049].some((mode) => params.includes(mode))) interrupt();
        return false;
      });
    }
    this.show(buffer, false);
    terminal.open(host);
    terminal.textarea?.setAttribute("tabindex", "-1");
    return buffer;
  }

  private show(buffer: Buffer, visible: boolean) {
    buffer.host.classList.toggle("peek-buffer-visible", visible);
    buffer.host.setAttribute("aria-hidden", String(!visible));
    buffer.host.inert = !visible;
  }

  private blank(terminal: Terminal) {
    const buffer = terminal.buffer.active;
    for (let y = 0; y < terminal.rows; y++) {
      if (
        buffer
          .getLine(buffer.baseY + y)
          ?.translateToString(true)
          .trim()
      )
        return false;
    }
    return true;
  }

  private async write(buffer: Buffer, paint: Paint) {
    const current = buffer.terminal;
    if (paint.kind === "reset") {
      current.reset();
      current.resize(paint.viewport.cols, paint.viewport.rows);
      buffer.interrupted = false;
    }
    for (let start = 0; start < paint.bytes.length; start += 64 * 1024) {
      if (this.disposed) throw new Error("Disposed terminal");
      await new Promise<void>((resolve, reject) => {
        const cancel = () => {
          this.pending.delete(cancel);
          reject(new Error("Disposed terminal"));
        };
        this.pending.add(cancel);
        current.write(paint.bytes.subarray(start, start + 64 * 1024), () => {
          this.pending.delete(cancel);
          if (this.disposed) reject(new Error("Disposed terminal"));
          else resolve();
        });
      });
    }
  }

  private async publish(generation: number, history: boolean) {
    const buffers = this.buffers;
    if (!buffers) return;
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => resolve()),
    );
    if (!this.active(generation)) return;
    if (this.hold?.timer) clearTimeout(this.hold.timer);
    this.hold = null;
    const [previous, next] = buffers;
    const focused = document.activeElement;
    const transferFocus = previous.host.contains(focused);
    this.show(previous, false);
    this.show(next, true);
    this.buffers = [next, previous];
    if (!next.initialized || transferFocus) {
      next.initialized = true;
      next.terminal.focus();
      if (!transferFocus) {
        next.terminal.blur();
        if (focused instanceof HTMLElement && focused.isConnected)
          focused.focus({ preventScroll: true });
      }
    }
    this.commit({
      terminal: next.terminal,
      baseline: this.baseline,
      interrupted: next.interrupted || this.skippedBlank,
      history,
    });
    if (history) {
      this.baseline = null;
      this.skippedBlank = false;
    }
    for (const paint of this.paints) await this.write(previous, paint);
    this.paints = [];
    next.interrupted = false;
    previous.interrupted = false;
  }

  dispose() {
    this.invalidate();
    this.disposed = true;
    for (const cancel of this.pending) cancel();
    this.pending.clear();
    for (const buffer of this.buffers ?? []) buffer.terminal.dispose();
    this.buffers = null;
    this.host.replaceChildren();
  }
}
