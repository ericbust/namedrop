export type WatchRow = Readonly<{ id: number; text: string }>;
export type WatchFrame = Readonly<{
  cols: number;
  buffer: "normal" | "alternate";
  rows: readonly string[];
}>;
export type WatchHistory = Readonly<{
  continuity: string | null;
  startedAt: number | null;
  nextId: number;
  seed: readonly WatchRow[];
  confirmed: readonly WatchRow[];
  baseline: WatchFrame | null;
}>;
export type WatchEvent =
  | Readonly<{
      kind: "baseline";
      continuity: string;
      at: number;
      frame: WatchFrame;
    }>
  | Readonly<{ kind: "frame"; frame: WatchFrame; interrupted?: boolean }>
  | Readonly<{ kind: "seed"; text: string }>
  | Readonly<{ kind: "detach" }>;

export const WATCH_LINE_LIMIT = 5000;
export const WATCH_BYTE_LIMIT = 1024 * 1024;
const encoder = new TextEncoder();

export function emptyWatch(): WatchHistory {
  return {
    continuity: null,
    startedAt: null,
    nextId: 1,
    seed: [],
    confirmed: [],
    baseline: null,
  };
}

function informative(text: string) {
  return /[\p{L}\p{N}]/u.test(text) && text.replace(/\s/gu, "").length >= 12;
}

function strong(
  rows: readonly string[],
  old: readonly string[],
  next: readonly string[],
) {
  const distinct = new Set(rows.filter(informative));
  if (distinct.size < 4) return false;
  let characters = 0;
  let unique = false;
  for (const row of distinct) {
    characters += row.replace(/\s/gu, "").length;
    if (
      old.filter((line) => line === row).length === 1 &&
      next.filter((line) => line === row).length === 1
    )
      unique = true;
  }
  return characters >= 80 && unique;
}

/** Only an exact, uniquely explained upward middle-band shift can add rows. */
export function departedRows(
  old: WatchFrame,
  next: WatchFrame,
): readonly string[] {
  if (
    old.cols !== next.cols ||
    old.buffer !== next.buffer ||
    old.rows.length !== next.rows.length
  )
    return [];
  const before = old.rows;
  const after = next.rows;
  const count = before.length;
  let first = 0;
  while (first < count && before[first] === after[first]) first++;
  if (first === count) return [];
  let last = count - 1;
  while (last > first && before[last] === after[last]) last--;
  let accepted: {
    start: number;
    shift: number;
    rows: readonly string[];
  } | null = null;
  for (let shift = 1; shift <= Math.floor(count / 2); shift++) {
    const mismatches = [0];
    for (let y = 0; y + shift < count; y++)
      mismatches.push(
        (mismatches[y] ?? 0) + (before[y + shift] === after[y] ? 0 : 1),
      );
    for (let start = 0; start <= first; start++) {
      for (
        let end = Math.max(last + 1, start + 2 * shift);
        end <= count;
        end++
      ) {
        const overlapEnd = end - shift;
        if (mismatches[overlapEnd] !== mismatches[start]) continue;
        if (!strong(before.slice(start + shift, end), before, after)) continue;
        const rows = before.slice(start, start + shift);
        if (!rows.some((row) => row.trim())) continue;
        if (accepted && (accepted.start !== start || accepted.shift !== shift))
          return [];
        accepted = { start, shift, rows };
      }
    }
  }
  return accepted?.rows ?? [];
}

function seedColumns(text: string) {
  let columns = 0;
  for (const symbol of text) {
    if (symbol === "\t") return Number.POSITIVE_INFINITY;
    columns += symbol.charCodeAt(0) <= 127 ? 1 : 2;
  }
  return columns;
}

function overlap(left: readonly string[], right: readonly string[]) {
  for (let size = Math.min(left.length, right.length); size >= 4; size--) {
    const tail = left.slice(-size);
    if (
      tail.every((text, index) => text === right[index]) &&
      strong(tail, left, right)
    )
      return size;
  }
  return 0;
}

export function watchRows(state: WatchHistory): readonly WatchRow[] {
  return [...state.seed, ...state.confirmed];
}

export function watchBytes(state: WatchHistory) {
  let bytes = 0;
  for (const row of watchRows(state))
    bytes += encoder.encode(row.text).length + 1;
  for (const text of state.baseline?.rows ?? [])
    bytes += encoder.encode(text).length + 1;
  return bytes;
}

function bounded(state: WatchHistory): WatchHistory {
  const seed = [...state.seed];
  const confirmed = [...state.confirmed];
  let bytes = watchBytes(state);
  while (
    seed.length + confirmed.length > WATCH_LINE_LIMIT ||
    bytes > WATCH_BYTE_LIMIT
  ) {
    const row = seed.length ? seed.shift() : confirmed.shift();
    if (!row) break;
    bytes -= encoder.encode(row.text).length + 1;
  }
  return { ...state, seed, confirmed };
}

function alignSeed(state: WatchHistory): WatchHistory {
  const seed = state.seed.map((row) => row.text);
  const confirmed = state.confirmed.map((row) => row.text);
  const captured = overlap(seed, confirmed);
  if (captured) return { ...state, seed: state.seed.slice(0, -captured) };
  if (state.confirmed.length || !state.baseline) return state;
  const visible = overlap(seed, state.baseline.rows);
  return visible ? { ...state, seed: state.seed.slice(0, -visible) } : state;
}

export function watchReduce(
  state: WatchHistory,
  event: WatchEvent,
): WatchHistory {
  switch (event.kind) {
    case "detach":
      return { ...state, baseline: null };
    case "baseline": {
      const same = state.continuity === event.continuity;
      const previous = same ? state : emptyWatch();
      return bounded(
        alignSeed({
          ...previous,
          continuity: event.continuity,
          startedAt: previous.startedAt ?? event.at,
          baseline: event.frame,
        }),
      );
    }
    case "frame": {
      const departed =
        state.baseline && !event.interrupted
          ? departedRows(state.baseline, event.frame)
          : [];
      let nextId = state.nextId;
      return bounded(
        alignSeed({
          ...state,
          confirmed: [
            ...state.confirmed,
            ...departed.map((text) => ({ id: nextId++, text })),
          ],
          nextId,
          baseline: event.frame,
        }),
      );
    }
    case "seed": {
      const source = event.text ? event.text.split("\n") : [];
      const cols = state.baseline?.cols;
      const wrapped =
        cols === undefined
          ? -1
          : source.findIndex((line) => seedColumns(line) > cols);
      const text = wrapped < 0 ? source : source.slice(0, wrapped);
      const previous = state.seed.map((row) => row.text);
      const unchanged =
        previous.length === text.length &&
        previous.every((row, i) => row === text[i]);
      if (
        state.confirmed.length &&
        !unchanged &&
        !overlap(
          text,
          state.confirmed.map((row) => row.text),
        )
      )
        return state;
      const shared =
        previous.length === text.length &&
        previous.every((row, i) => row === text[i])
          ? previous.length
          : overlap(previous, text);
      let nextId = state.nextId;
      const retained = shared ? state.seed.slice(-shared) : [];
      const seed = [
        ...retained,
        ...text.slice(shared).map((line) => ({ id: nextId++, text: line })),
      ];
      return bounded(alignSeed({ ...state, seed, nextId }));
    }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
