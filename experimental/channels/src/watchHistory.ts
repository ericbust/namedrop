type TextRow = Readonly<{ kind: "text"; id: number; text: string }>;
export type WatchRow =
  | TextRow
  | Readonly<{ kind: "gap"; id: number; text: "… some lines missed" }>;
type Occurrence =
  | Readonly<{ kind: "candidate"; row: TextRow }>
  | Readonly<{ kind: "positioned"; row: TextRow }>
  | Readonly<{
      kind: "conflicted";
      id: number;
      alternatives: readonly string[];
    }>
  | Readonly<{ kind: "missing"; id: number }>
  | Readonly<{ kind: "published"; row: TextRow }>;
type Ledger =
  | Readonly<{ kind: "unlocated"; boundary: "continuous" | "gap" }>
  | Readonly<{
      kind: "located";
      frame: WatchFrame | null;
      start: number;
      end: number;
      layout: "physical" | "spaced";
      screen: Readonly<{
        cols: number;
        buffer: WatchFrame["buffer"];
        height: number;
      }>;
      origin: number;
      top: number;
      publishedTo: number;
      cells: readonly Occurrence[];
      boundary: "continuous" | "gap";
    }>;
type Scroll = Readonly<{ start: number; end: number; shift: number }>;
type Alignment =
  | Readonly<{ kind: "matched"; offset: number }>
  | Readonly<{ kind: "unmatched" }>
  | Readonly<{ kind: "ambiguous" }>;
export type WatchFrame = Readonly<{
  cols: number;
  buffer: "normal" | "alternate";
  rows: readonly string[];
}>;
export type WatchHistory = Readonly<{
  continuity: string | null;
  startedAt: number | null;
  nextId: number;
  seed: readonly TextRow[];
  confirmed: readonly WatchRow[];
  baseline: WatchFrame | null;
  ledger: Ledger;
  segments: readonly Extract<Ledger, { kind: "located" }>[];
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
    ledger: { kind: "unlocated", boundary: "continuous" },
    segments: [],
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

function scrollBetween(old: WatchFrame, next: WatchFrame): Scroll | null {
  if (
    old.cols !== next.cols ||
    old.buffer !== next.buffer ||
    old.rows.length !== next.rows.length
  )
    return null;
  const before = old.rows;
  const after = next.rows;
  const count = before.length;
  let first = 0;
  while (first < count && before[first] === after[first]) first++;
  if (first === count) return null;
  let last = count - 1;
  while (last > first && before[last] === after[last]) last--;
  let accepted: Scroll | null = null;
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
          return null;
        accepted = { start, end, shift };
      }
    }
  }
  return accepted;
}

export function departedRows(
  old: WatchFrame,
  next: WatchFrame,
): readonly string[] {
  const scroll = scrollBetween(old, next);
  return scroll
    ? old.rows.slice(scroll.start, scroll.start + scroll.shift)
    : [];
}

function occurrenceText(cell: Occurrence) {
  return cell.kind === "conflicted" || cell.kind === "missing"
    ? null
    : cell.row.text;
}

function visibleRows(
  ledger: Extract<Ledger, { kind: "located" }>,
  frame: WatchFrame,
) {
  const rows = frame.rows.slice(ledger.start, ledger.end);
  return ledger.layout === "spaced" ? rows.filter((text) => text.trim()) : rows;
}

function withoutLayoutBlanks(
  ledger: Extract<Ledger, { kind: "located" }>,
): Extract<Ledger, { kind: "located" }> {
  const keep = (cell: Occurrence) => occurrenceText(cell)?.trim() !== "";
  return {
    ...ledger,
    layout: "spaced",
    origin: 0,
    top: ledger.cells.slice(0, ledger.top - ledger.origin).filter(keep).length,
    publishedTo: ledger.cells
      .slice(0, ledger.publishedTo - ledger.origin)
      .filter(keep).length,
    cells: ledger.cells.filter(keep),
  };
}

function uniqueAlignment(
  known: readonly (string | null)[],
  rows: readonly string[],
): Alignment {
  const index = new Map<string, number[]>();
  for (const [i, text] of known.entries()) {
    if (text === null || !informative(text)) continue;
    const positions = index.get(text) ?? [];
    positions.push(i);
    index.set(text, positions);
  }
  const candidates = new Set<number>();
  for (const [y, text] of rows.entries()) {
    for (const position of index.get(text) ?? []) candidates.add(position - y);
  }
  let accepted: number | null = null;
  for (const top of candidates) {
    let run: string[] = [];
    let anchored = false;
    for (let y = 0; y <= rows.length; y++) {
      const previous = known[top + y];
      const text = rows[y];
      if (text !== undefined && text === previous) {
        run.push(text);
      } else {
        if (strong(run, run, run)) anchored = true;
        run = [];
      }
    }
    if (!anchored) continue;
    if (accepted !== null && accepted !== top) return { kind: "ambiguous" };
    accepted = top;
  }
  return accepted === null
    ? { kind: "unmatched" }
    : { kind: "matched", offset: accepted };
}

function locate(
  ledger: Extract<Ledger, { kind: "located" }>,
  frame: WatchFrame,
) {
  const alignment = uniqueAlignment(
    ledger.cells.map(occurrenceText),
    visibleRows(ledger, frame),
  );
  return alignment.kind === "matched"
    ? ({
        kind: "matched",
        offset: ledger.origin + alignment.offset,
      } satisfies Alignment)
    : alignment;
}

function observeOccurrence(cell: Occurrence, text: string): Occurrence {
  if (cell.kind === "missing") return cell;
  if (cell.kind === "published") {
    return text.startsWith(cell.row.text) && text !== cell.row.text
      ? { kind: "published", row: { ...cell.row, text } }
      : cell;
  }
  if (cell.kind === "candidate" || cell.kind === "positioned") {
    if (cell.row.text === text || cell.row.text.startsWith(text)) return cell;
    if (text.startsWith(cell.row.text))
      return { kind: "candidate", row: { ...cell.row, text } };
    return {
      kind: "conflicted",
      id: cell.row.id,
      alternatives: [cell.row.text, text],
    };
  }
  const compatible = cell.alternatives.filter(
    (candidate) => candidate === text || text.startsWith(candidate),
  );
  if (compatible.length === 1)
    return { kind: "candidate", row: { kind: "text", id: cell.id, text } };
  const alternatives = [...new Set([...cell.alternatives, text])];
  return alternatives.length <= 8
    ? { ...cell, alternatives }
    : { kind: "missing", id: cell.id };
}

function observeFrame(
  state: WatchHistory,
  frame: WatchFrame,
  interrupted: boolean,
): WatchHistory {
  const previous = state.baseline;
  if (interrupted || !previous)
    return {
      ...state,
      baseline: frame,
      ledger: { ...state.ledger, boundary: "gap" },
    };
  const screen =
    state.ledger.kind === "located"
      ? state.ledger.screen
      : {
          cols: previous.cols,
          buffer: previous.buffer,
          height: previous.rows.length,
        };
  if (
    screen.cols !== frame.cols ||
    screen.buffer !== frame.buffer ||
    screen.height !== frame.rows.length
  )
    return {
      ...state,
      baseline: frame,
      ledger: { kind: "unlocated", boundary: "gap" },
      segments:
        state.ledger.kind === "located"
          ? [...state.segments, { ...state.ledger, frame: null }]
          : state.segments,
    };
  let nextId = state.nextId;
  let ledger = state.ledger;
  const segments = [...state.segments];
  const scroll = scrollBetween(previous, frame);
  const backward = scrollBetween(frame, previous);
  let alignment: Alignment = { kind: "unmatched" };
  if (ledger.kind === "located" && ledger.frame === previous) {
    const movement = scroll ?? backward;
    if (
      movement &&
      ledger.layout === "physical" &&
      movement.start >= ledger.start &&
      movement.end === ledger.end
    ) {
      ledger = {
        ...ledger,
        start: movement.start,
        top: ledger.top + movement.start - ledger.start,
      };
      alignment = {
        kind: "matched",
        offset: ledger.top + (scroll ? movement.shift : -movement.shift),
      };
    }
  }
  if (alignment.kind === "unmatched") {
    if (ledger.kind === "located" && scroll && scroll.start > ledger.start)
      ledger = {
        ...ledger,
        start: scroll.start,
        top: ledger.top + scroll.start - ledger.start,
      };
    let matched: Extract<Ledger, { kind: "located" }> | null = null;
    let matchedIndex = -1;
    const candidates =
      ledger.kind === "located" ? [...segments, ledger] : segments;
    for (const [index, candidate] of candidates.entries()) {
      if (
        candidate.screen.cols !== frame.cols ||
        candidate.screen.buffer !== frame.buffer ||
        candidate.screen.height !== frame.rows.length
      )
        continue;
      let located = candidate;
      let found = locate(located, frame);
      if (found.kind === "unmatched" && candidate.layout === "physical") {
        const spaced = withoutLayoutBlanks(candidate);
        const spacedAlignment = locate(spaced, frame);
        if (spacedAlignment.kind !== "unmatched") {
          located = spaced;
          found = spacedAlignment;
        }
      }
      if (
        found.kind === "ambiguous" ||
        (found.kind === "matched" && matched !== null)
      ) {
        alignment = { kind: "ambiguous" };
        break;
      }
      if (found.kind === "matched") {
        matched = located;
        matchedIndex = index;
        alignment = found;
      }
    }
    if (alignment.kind === "matched" && matched) {
      if (matchedIndex < segments.length) {
        segments.splice(matchedIndex, 1);
        if (ledger.kind === "located")
          segments.push({ ...ledger, frame: null });
      }
      ledger = matched;
    }
  }
  if (alignment.kind === "ambiguous") {
    const confirmed =
      state.confirmed.at(-1)?.kind === "gap"
        ? state.confirmed
        : [
            ...state.confirmed,
            {
              kind: "gap",
              id: nextId++,
              text: "… some lines missed",
            } satisfies WatchRow,
          ];
    return { ...state, confirmed, nextId, baseline: frame };
  }
  let top = alignment.kind === "matched" ? alignment.offset : null;
  if (top === null) {
    if (!scroll) return { ...state, baseline: frame };
    const boundary = ledger.kind === "unlocated" ? ledger.boundary : "gap";
    if (ledger.kind === "located") segments.push({ ...ledger, frame: null });
    ledger = {
      kind: "located",
      frame: previous,
      start: scroll.start,
      end: scroll.end,
      layout: "physical",
      screen: {
        cols: frame.cols,
        buffer: frame.buffer,
        height: frame.rows.length,
      },
      origin: 0,
      top: 0,
      publishedTo: 0,
      cells: previous.rows.slice(scroll.start, scroll.end).map(
        (text): Occurrence => ({
          kind: "candidate",
          row: { kind: "text", id: nextId++, text },
        }),
      ),
      boundary,
    };
    top = scroll.shift;
  }
  if (ledger.kind !== "located") return { ...state, baseline: frame };
  const cells = [...ledger.cells];
  const rows = visibleRows(ledger, frame);
  for (const [y, text] of rows.entries()) {
    const position = top + y;
    const i = position - ledger.origin;
    const cell = cells[i];
    if (i < 0) continue;
    cells[i] = cell
      ? observeOccurrence(cell, text)
      : { kind: "candidate", row: { kind: "text", id: nextId++, text } };
  }
  for (let y = 0; y + 1 < rows.length; y++) {
    const i = top + y - ledger.origin;
    const cell = cells[i];
    if (cell?.kind === "candidate")
      cells[i] = { kind: "positioned", row: cell.row };
  }
  let commitTo = top;
  if (top < ledger.top) {
    for (let i = top + rows.length - ledger.origin; i < cells.length; i++)
      if (cells[i]?.kind === "positioned") commitTo = ledger.origin + i + 1;
  }
  const replacements = new Map<number, TextRow>();
  for (const cell of cells)
    if (cell.kind === "published") replacements.set(cell.row.id, cell.row);
  const confirmed = state.confirmed.map(
    (row) => replacements.get(row.id) ?? row,
  );
  let boundary = ledger.boundary;
  function gap() {
    if (confirmed.at(-1)?.kind !== "gap")
      confirmed.push({
        kind: "gap",
        id: nextId++,
        text: "… some lines missed",
      });
  }
  if (commitTo > ledger.publishedTo && boundary === "gap") gap();
  for (let position = ledger.publishedTo; position < commitTo; position++) {
    const i = position - ledger.origin;
    const cell = cells[i];
    if (!cell || cell.kind === "conflicted" || cell.kind === "missing") gap();
    else if (cell.kind !== "published") {
      confirmed.push(cell.row);
      cells[i] = { kind: "published", row: cell.row };
    }
    boundary = "continuous";
  }
  return {
    ...state,
    nextId,
    confirmed,
    baseline: frame,
    segments,
    ledger: {
      ...ledger,
      frame,
      top,
      cells,
      boundary,
      publishedTo: Math.max(ledger.publishedTo, commitTo),
    },
  };
}

function seedColumns(text: string) {
  let columns = 0;
  for (const symbol of text) {
    if (symbol === "\t") return Number.POSITIVE_INFINITY;
    columns += symbol.charCodeAt(0) <= 127 ? 1 : 2;
  }
  return columns;
}

export function watchRows(state: WatchHistory): readonly WatchRow[] {
  return [...state.seed, ...state.confirmed];
}

function occurrenceBytes(cell: Occurrence) {
  const texts =
    cell.kind === "missing"
      ? []
      : cell.kind === "conflicted"
        ? cell.alternatives
        : [cell.row.text];
  return texts.reduce(
    (bytes, text) => bytes + encoder.encode(text).length + 1,
    0,
  );
}

export function watchBytes(state: WatchHistory) {
  let bytes = 0;
  for (const row of watchRows(state))
    bytes += encoder.encode(row.text).length + 1;
  const segments =
    state.ledger.kind === "located"
      ? [...state.segments, state.ledger]
      : state.segments;
  for (const segment of segments) {
    for (const cell of segment.cells)
      if (cell.kind !== "published") bytes += occurrenceBytes(cell);
  }
  for (const text of state.baseline?.rows ?? [])
    bytes += encoder.encode(text).length + 1;
  return bytes;
}

function bounded(state: WatchHistory): WatchHistory {
  const seed = [...state.seed];
  const confirmed = [...state.confirmed];
  let ledger = state.ledger;
  let segments = [...state.segments];
  let bytes = watchBytes(state);
  while (segments.length && bytes > WATCH_BYTE_LIMIT) {
    const segment = segments.shift();
    for (const cell of segment?.cells ?? [])
      if (cell.kind !== "published") bytes -= occurrenceBytes(cell);
  }
  while (
    seed.length + confirmed.length > WATCH_LINE_LIMIT ||
    bytes > WATCH_BYTE_LIMIT
  ) {
    const row = seed.length ? seed.shift() : confirmed.shift();
    if (!row) break;
    bytes -= encoder.encode(row.text).length + 1;
  }
  if (ledger.kind === "located") {
    const retained = new Set(confirmed.map((row) => row.id));
    let cut = 0;
    while (ledger.origin + cut < ledger.publishedTo) {
      const cell = ledger.cells[cut];
      if (cell && cell.kind === "published" && retained.has(cell.row.id)) break;
      cut++;
    }
    ledger = {
      ...ledger,
      origin: ledger.origin + cut,
      cells: ledger.cells.slice(cut),
    };
    if (bytes > WATCH_BYTE_LIMIT)
      ledger = { kind: "unlocated", boundary: "gap" };
  }
  const retained = new Set(confirmed.map((row) => row.id));
  segments = segments.flatMap((segment) => {
    const first = segment.cells.findIndex(
      (cell) => cell.kind === "published" && retained.has(cell.row.id),
    );
    return first < 0
      ? []
      : [
          {
            ...segment,
            origin: segment.origin + first,
            cells: segment.cells.slice(first),
          },
        ];
  });
  return { ...state, seed, confirmed, ledger, segments };
}

function seedBoundary(state: WatchHistory, source: readonly string[]) {
  let known = state.confirmed.map((row) =>
    row.kind === "text" ? row.text : null,
  );
  if (state.ledger.kind === "located") {
    const ids = new Set(
      state.ledger.cells.flatMap((cell) =>
        cell.kind === "conflicted" || cell.kind === "missing"
          ? []
          : [cell.row.id],
      ),
    );
    if (state.confirmed.every((row) => ids.has(row.id)))
      known = state.ledger.cells.map(occurrenceText);
  } else if (!state.confirmed.length) {
    known = [...(state.baseline?.rows ?? [])];
  }
  if (known.some((text) => text === null)) return null;
  const visible = known.flatMap((text) => (text === null ? [] : [text]));
  const alignment = uniqueAlignment(source, visible);
  if (alignment.kind !== "matched") return null;
  const offset = alignment.offset;
  if (state.ledger.kind === "located" || state.confirmed.length) return offset;
  let run: string[] = [];
  for (let i = 0; i <= visible.length; i++) {
    const text = visible[i];
    if (text !== undefined && source[i + offset] === text) run.push(text);
    else {
      if (strong(run, run, run)) return offset + i - run.length;
      run = [];
    }
  }
  return null;
}

function alignSeed(state: WatchHistory): WatchHistory {
  const boundary = seedBoundary(
    state,
    state.seed.map((row) => row.text),
  );
  return boundary === null
    ? state
    : { ...state, seed: state.seed.slice(0, Math.max(0, boundary)) };
}

export function watchReduce(
  state: WatchHistory,
  event: WatchEvent,
): WatchHistory {
  if (event.kind === "frame" || event.kind === "baseline") {
    const rows = event.frame.rows.map((text) => text.trimEnd());
    event = { ...event, frame: { ...event.frame, rows } };
  }
  switch (event.kind) {
    case "detach":
      return {
        ...state,
        baseline: null,
        ledger: { ...state.ledger, boundary: "gap" },
      };
    case "baseline": {
      const same = state.continuity === event.continuity;
      const previous = same ? state : emptyWatch();
      const resumed = {
        ...previous,
        continuity: event.continuity,
        startedAt: previous.startedAt ?? event.at,
        baseline: event.frame,
      };
      return bounded(
        alignSeed(
          same && previous.ledger.kind === "located"
            ? observeFrame(resumed, event.frame, false)
            : resumed,
        ),
      );
    }
    case "frame":
      return bounded(
        alignSeed(observeFrame(state, event.frame, event.interrupted ?? false)),
      );
    case "seed": {
      const source = event.text ? event.text.split("\n") : [];
      const cols = state.baseline?.cols;
      const wrapped =
        cols === undefined
          ? -1
          : source.findIndex((line) => seedColumns(line) > cols);
      const text = wrapped < 0 ? source : source.slice(0, wrapped);
      const boundary = seedBoundary(state, text);
      const incoming =
        boundary === null ? text : text.slice(0, Math.max(0, boundary));
      const previous = state.seed.map((row) => row.text);
      const unchanged =
        previous.length === incoming.length &&
        previous.every((row, i) => row === incoming[i]);
      if (state.confirmed.length && !unchanged && boundary === null)
        return state;
      const alignment = uniqueAlignment(previous, incoming);
      const shared = unchanged
        ? 0
        : alignment.kind === "matched"
          ? alignment.offset
          : null;
      let nextId = state.nextId;
      const seed = incoming.map((text, i): TextRow => {
        const previousRow =
          shared === null ? undefined : state.seed[i + shared];
        return previousRow?.kind === "text" && previousRow.text === text
          ? previousRow
          : { kind: "text", id: nextId++, text };
      });
      return bounded(alignSeed({ ...state, seed, nextId }));
    }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
