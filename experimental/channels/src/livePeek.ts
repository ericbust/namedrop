import type { PeekTarget } from "./model.ts";

export type LiveUpdate =
  | Readonly<{
      kind: "reset";
      continuity: string;
      viewport: Readonly<{ cols: number; rows: number }>;
      bytes: Uint8Array;
    }>
  | Readonly<{ kind: "data"; bytes: Uint8Array }>
  | Readonly<{ kind: "history"; text: string }>;

export type LiveState =
  | Readonly<{ kind: "connecting" | "reconnecting" }>
  | Readonly<{ kind: "live"; continuity: string }>
  | Readonly<{ kind: "closed"; reason: "ended" | "replaced" | "missing" }>
  | Readonly<{ kind: "session-ended" }>
  | Readonly<{
      kind: "unavailable";
      reason: "unsupported" | "busy" | "interrupted" | "invalid_source";
    }>;

type Options = Readonly<{
  target: PeekTarget;
  signal: AbortSignal;
  retainedContinuity?: string | null;
  apply(update: LiveUpdate): Promise<void>;
  onState(state: LiveState): void;
}>;
type Final = Exclude<
  LiveState,
  { kind: "connecting" | "live" | "reconnecting" }
>;
type Outcome = Final | { kind: "retry"; reason: "busy" | "interrupted" };
type Wire =
  | {
      kind: "reset";
      continuity: string;
      viewport: { cols: number; rows: number };
      bytes: Uint8Array;
    }
  | { kind: "data"; bytes: Uint8Array }
  | { kind: "history"; text: string }
  | { kind: "closed"; reason: "ended" | "replaced" | "missing" }
  | {
      kind: "unavailable";
      reason:
        | "unsupported"
        | "busy"
        | "source_lost"
        | "invalid_source"
        | "slow_viewer"
        | "server_stopping";
    };

const LINE_LIMIT = 2 * 1024 * 1024;
const PAINT_LIMIT = 1024 * 1024;
const DATA_LIMIT = 64 * 1024;
const HISTORY_LIMIT = 1024 * 1024;
const HISTORY_LINES = 2200;
const WATCHDOG_MS = 30_000;
const RETRIES = [1000, 2000, 4000];
class InvalidSource extends Error {}
class Interrupted extends Error {}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function exact(record: Record<string, unknown>, fields: string[]) {
  if (
    Object.keys(record).length !== fields.length ||
    fields.some((key) => !(key in record))
  )
    throw new InvalidSource();
}
function integer(value: unknown, max: number): value is number {
  return (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 1 &&
    value <= max
  );
}
function bytes(value: unknown, limit: number): Uint8Array {
  if (
    typeof value !== "string" ||
    value.length > Math.ceil(limit / 3) * 4 ||
    value.length % 4 !== 0 ||
    !/^[A-Za-z0-9+/]*={0,2}$/.test(value)
  )
    throw new InvalidSource();
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    throw new InvalidSource();
  }
  if (binary.length > limit || btoa(binary) !== value)
    throw new InvalidSource();
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}
function parse(line: Uint8Array, seq: number): Wire {
  let raw: unknown;
  try {
    raw = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(line));
  } catch {
    throw new InvalidSource();
  }
  if (!isRecord(raw) || raw.v !== 1 || raw.seq !== seq)
    throw new InvalidSource();
  switch (raw.kind) {
    case "reset": {
      exact(raw, ["v", "seq", "kind", "continuity", "cols", "rows", "bytes"]);
      if (
        typeof raw.continuity !== "string" ||
        !/^[A-Za-z0-9_-]{1,256}$/.test(raw.continuity) ||
        !integer(raw.cols, 512) ||
        !integer(raw.rows, 256) ||
        raw.cols * raw.rows > 65536
      )
        throw new InvalidSource();
      return {
        kind: "reset",
        continuity: raw.continuity,
        viewport: { cols: raw.cols, rows: raw.rows },
        bytes: bytes(raw.bytes, PAINT_LIMIT),
      };
    }
    case "data":
      exact(raw, ["v", "seq", "kind", "bytes"]);
      return { kind: "data", bytes: bytes(raw.bytes, DATA_LIMIT) };
    case "history": {
      exact(raw, ["v", "seq", "kind", "text"]);
      if (
        typeof raw.text !== "string" ||
        new TextEncoder().encode(raw.text).length > HISTORY_LIMIT ||
        raw.text.split("\n").length > HISTORY_LINES
      )
        throw new InvalidSource();
      return { kind: "history", text: raw.text };
    }
    case "closed":
      exact(raw, ["v", "seq", "kind", "reason"]);
      if (
        raw.reason !== "ended" &&
        raw.reason !== "replaced" &&
        raw.reason !== "missing"
      )
        throw new InvalidSource();
      return { kind: "closed", reason: raw.reason };
    case "unavailable":
      exact(raw, ["v", "seq", "kind", "reason"]);
      switch (raw.reason) {
        case "unsupported":
        case "busy":
        case "source_lost":
        case "invalid_source":
        case "slow_viewer":
        case "server_stopping":
          return { kind: "unavailable", reason: raw.reason };
        default:
          throw new InvalidSource();
      }
    default:
      throw new InvalidSource();
  }
}

function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(new Interrupted());
  return new Promise((resolve, reject) => {
    const stop = () => reject(new Interrupted());
    signal.addEventListener("abort", stop, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener("abort", stop);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", stop);
        reject(error);
      },
    );
  });
}

/** One observation owns continuity and its entire retry budget until disposed. */
export async function observePeek({
  target,
  signal,
  retainedContinuity = null,
  apply,
  onState,
}: Options): Promise<void> {
  if (signal.aborted) return;
  let continuity: string | null = retainedContinuity;
  let observed = false;
  onState({ kind: "connecting" });

  async function attempt(): Promise<Outcome> {
    const controller = new AbortController();
    const stop = () => controller.abort();
    signal.addEventListener("abort", stop, { once: true });
    let timer: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      clearTimeout(timer);
      timer = setTimeout(stop, WATCHDOG_MS);
    };
    let response: Response | undefined;
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    arm();
    try {
      const path = `/w/${target.channel}/participants/${encodeURIComponent(target.member)}/live`;
      response = await abortable(
        fetch(
          continuity === null
            ? path
            : `${path}?continuity=${encodeURIComponent(continuity)}`,
          {
            credentials: "same-origin",
            cache: "no-store",
            redirect: "error",
            headers: { Accept: "application/x-ndjson" },
            signal: controller.signal,
          },
        ),
        controller.signal,
      );
      if (response.status === 401 || response.status === 403)
        return { kind: "session-ended" };
      if (response.status === 404) return { kind: "closed", reason: "missing" };
      if (response.status === 410)
        return { kind: "closed", reason: "replaced" };
      if (response.status === 503) return { kind: "retry", reason: "busy" };
      if (
        !response.ok ||
        response.headers
          .get("content-type")
          ?.split(";")[0]
          ?.trim()
          .toLowerCase() !== "application/x-ndjson" ||
        response.body === null
      )
        throw new InvalidSource();
      reader = response.body.getReader();
      let buffer = new Uint8Array(4096);
      let used = 0;
      let seq = 1;
      let reset = false;
      for (;;) {
        const chunk = await abortable(reader.read(), controller.signal);
        if (chunk.done) return { kind: "retry", reason: "interrupted" };
        arm();
        let start = 0;
        while (start < chunk.value.length) {
          const newline = chunk.value.indexOf(10, start);
          const end = newline < 0 ? chunk.value.length : newline;
          const length = end - start;
          if (used + length > LINE_LIMIT) throw new InvalidSource();
          if (used + length > buffer.length) {
            const next = new Uint8Array(
              Math.min(LINE_LIMIT, Math.max(buffer.length * 2, used + length)),
            );
            next.set(buffer.subarray(0, used));
            buffer = next;
          }
          buffer.set(chunk.value.subarray(start, end), used);
          used += length;
          start = newline < 0 ? chunk.value.length : newline + 1;
          if (newline < 0) continue;
          if (used === 0 || (used === 1 && buffer[0] === 13)) {
            used = 0;
            continue;
          }
          const record = parse(buffer.subarray(0, used), seq++);
          used = 0;
          if (controller.signal.aborted) throw new Interrupted();
          switch (record.kind) {
            case "reset":
              if (continuity !== null && continuity !== record.continuity)
                return { kind: "closed", reason: "replaced" };
              continuity = record.continuity;
              observed = true;
              reset = true;
              await abortable(
                apply({
                  kind: "reset",
                  continuity: record.continuity,
                  viewport: record.viewport,
                  bytes: record.bytes,
                }),
                controller.signal,
              );
              if (!signal.aborted)
                onState({ kind: "live", continuity: record.continuity });
              break;
            case "data":
              if (!reset) throw new InvalidSource();
              await abortable(
                apply({ kind: "data", bytes: record.bytes }),
                controller.signal,
              );
              break;
            case "history":
              if (!reset) throw new InvalidSource();
              await abortable(
                apply({ kind: "history", text: record.text }),
                controller.signal,
              );
              break;
            case "closed":
              return record;
            case "unavailable":
              switch (record.reason) {
                case "source_lost":
                case "slow_viewer":
                case "server_stopping":
                  return { kind: "retry", reason: "interrupted" };
                case "unsupported":
                  return {
                    kind: "unavailable",
                    reason: observed ? "invalid_source" : "unsupported",
                  };
                case "busy":
                case "invalid_source":
                  return { kind: "unavailable", reason: record.reason };
                default: {
                  const exhaustive: never = record;
                  return exhaustive;
                }
              }
            default: {
              const exhaustive: never = record;
              return exhaustive;
            }
          }
        }
      }
    } catch (error) {
      return error instanceof InvalidSource
        ? { kind: "unavailable", reason: "invalid_source" }
        : { kind: "retry", reason: "interrupted" };
    } finally {
      clearTimeout(timer);
      signal.removeEventListener("abort", stop);
      controller.abort();
      if (reader) {
        void reader.cancel().catch(() => {});
        reader.releaseLock();
      } else {
        void response?.body?.cancel().catch(() => {});
      }
    }
  }

  for (let retry = 0; !signal.aborted; retry++) {
    const outcome = await attempt();
    if (signal.aborted) return;
    if (outcome.kind !== "retry") {
      onState(outcome);
      return;
    }
    const delay = RETRIES[retry];
    if (delay === undefined) {
      onState({ kind: "unavailable", reason: outcome.reason });
      return;
    }
    onState({ kind: "reconnecting" });
    await new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal.removeEventListener("abort", done);
        resolve();
      };
      const timer = setTimeout(done, delay);
      signal.addEventListener("abort", done, { once: true });
      if (signal.aborted) done();
    });
  }
}
