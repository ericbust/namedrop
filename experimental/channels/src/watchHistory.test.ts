import assert from "node:assert/strict";
import { test } from "node:test";
import prose from "./fixtures/watch-history/codex-prose.json" with {
  type: "json",
};
import recorded from "./fixtures/watch-history/recorded-cases.json" with {
  type: "json",
};
import {
  departedRows,
  emptyWatch,
  WATCH_BYTE_LIMIT,
  WATCH_LINE_LIMIT,
  type WatchFrame,
  watchBytes,
  watchReduce,
  watchRows,
} from "./watchHistory.ts";

const line = (n: number) =>
  `  Unique content row ${String(n).padStart(5, "0")} with enough evidence`;
const frame = (start = 0, count = 12): WatchFrame => ({
  cols: 100,
  buffer: "normal",
  rows: [
    "Fixed header",
    ...Array.from({ length: count }, (_, i) => line(start + i)),
    "Fixed footer",
  ],
});
const baseline = (current = frame()) =>
  watchReduce(emptyWatch(), {
    kind: "baseline",
    continuity: "pane-one",
    at: 1000,
    frame: current,
  });

test("exact middle-band upward scroll captures only departed rows, preserving indent and blanks", () => {
  assert.deepEqual(departedRows(frame(), frame(2)), [line(0), line(1)]);
  const before = frame();
  const old = {
    ...before,
    rows: ["Fixed header", line(0), "", ...before.rows.slice(3)],
  };
  const after = {
    ...old,
    rows: [
      "Fixed header",
      ...old.rows.slice(3, -1),
      line(12),
      line(13),
      "Fixed footer",
    ],
  };
  assert.deepEqual(departedRows(old, after), [line(0), ""]);
  const state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  assert.deepEqual(
    watchRows(state).map((row) => row.text),
    [line(0), line(1)],
  );
  assert.equal(state.startedAt, 1000);
});

test("screen-edge scrolling needs the same exact evidence as a fixed middle band", () => {
  const before = { ...frame(), rows: frame().rows.slice(1, -1) };
  const after = { ...frame(1), rows: frame(1).rows.slice(1, -1) };
  assert.deepEqual(departedRows(before, after), [line(0)]);
});

test("redraw, clear, resize, buffer switch, spinners and sparse or repetitive matches add no history", () => {
  const before = frame();
  const negatives: WatchFrame[] = [
    before,
    { ...before, cols: 99 },
    { ...before, rows: before.rows.slice(1) },
    { ...before, buffer: "alternate" },
    { ...before, rows: before.rows.map(() => "") },
    {
      ...before,
      rows: before.rows.map((row, i) => (i === 8 ? "Working ⠋" : row)),
    },
    { ...before, rows: [...before.rows].reverse() },
    frame(40),
    {
      ...frame(2),
      rows: frame(2).rows.map((row, i) =>
        i === 6 ? "An unrelated redraw" : row,
      ),
    },
    { ...frame(2), rows: [...frame(2).rows.slice(0, -1), "Changed footer"] },
  ];
  for (const next of negatives)
    assert.deepEqual(departedRows(before, next), []);
  const repetitive = {
    ...before,
    rows: ["header", ...Array(12).fill(line(1)), "footer"],
  };
  assert.deepEqual(
    departedRows(repetitive, {
      ...repetitive,
      rows: ["header", ...Array(11).fill(line(1)), line(2), "footer"],
    }),
    [],
  );
  const sparse = {
    ...before,
    rows: ["header", "a", "b", "c", "d", "e", "f", "footer"],
  };
  assert.deepEqual(
    departedRows(sparse, {
      ...sparse,
      rows: ["header", "b", "c", "d", "e", "f", "g", "footer"],
    }),
    [],
  );
  assert.deepEqual(
    watchRows(
      watchReduce(baseline(), {
        kind: "frame",
        frame: frame(2),
        interrupted: true,
      }),
    ),
    [],
  );
});

test("a rejected frame becomes the next baseline rather than bridging ambiguous output", () => {
  let state = baseline();
  state = watchReduce(state, { kind: "frame", frame: frame(40) });
  state = watchReduce(state, { kind: "frame", frame: frame(42) });
  assert.deepEqual(
    state.confirmed.map((row) => row.text),
    [line(40), line(41)],
  );
});

test("reset and detach preserve history and time while forbidding inference across the gap", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  const rows = state.confirmed;
  state = watchReduce(state, { kind: "detach" });
  assert.equal(state.baseline, null);
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 2000,
    frame: frame(70),
  });
  assert.deepEqual(state.confirmed, rows);
  assert.equal(state.startedAt, 1000);
  state = watchReduce(state, { kind: "frame", frame: frame(72) });
  assert.deepEqual(
    state.confirmed.map((row) => row.text),
    [line(0), line(1), "… some lines missed", line(70), line(71)],
  );
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "replacement",
    at: 3000,
    frame: frame(80),
  });
  assert.deepEqual(watchRows(state), []);
  assert.equal(state.startedAt, 3000);
});

test("a same-continuity rebaseline reuses uniquely anchored occurrences", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 2000,
    frame: frame(),
  });
  state = watchReduce(state, { kind: "frame", frame: frame(2) });
  const rows = watchRows(state);
  assert.deepEqual(
    rows.slice(0, 2).map((row) => row.text),
    [line(0), line(1)],
  );
  assert.equal(new Set(rows.map((row) => row.id)).size, rows.length);
  assert.equal(rows.filter((row) => row.text === line(0)).length, 1);
  assert.equal(rows.filter((row) => row.text === line(1)).length, 1);
});

test("seed replacement keeps occurrence anchors and strong visible overlap appears once", () => {
  let state = baseline({
    ...frame(),
    rows: Array.from({ length: 12 }, (_, i) => line(i + 10)),
  });
  state = watchReduce(state, {
    kind: "seed",
    text: Array.from({ length: 16 }, (_, i) => line(i)).join("\n"),
  });
  assert.deepEqual(
    state.seed.map((row) => row.text),
    Array.from({ length: 10 }, (_, i) => line(i)),
  );
  const firstIds = state.seed.map((row) => row.id);
  state = watchReduce(state, {
    kind: "seed",
    text: Array.from({ length: 16 }, (_, i) => line(i)).join("\n"),
  });
  assert.deepEqual(
    state.seed.map((row) => row.id),
    firstIds,
  );
  let rotating = watchReduce(baseline(), {
    kind: "seed",
    text: Array.from({ length: 100 }, (_, i) => line(100 + i)).join("\n"),
  });
  const anchor = rotating.seed[40];
  rotating = watchReduce(rotating, {
    kind: "seed",
    text: Array.from({ length: 100 }, (_, i) => line(120 + i)).join("\n"),
  });
  assert.deepEqual(rotating.seed[20], anchor);
  assert.equal(rotating.seed.length, 100);
});

test("watched history survives ambiguous rotating seeds and never globally deduplicates equal rows", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  const confirmed = state.confirmed;
  state = watchReduce(state, { kind: "seed", text: "unrelated seed" });
  assert.deepEqual(state.confirmed, confirmed);
});

test("line and encoded byte budgets evict oldest rows with stable IDs for survivors", () => {
  let state = baseline();
  for (let start = 0; start < 5100; start += 4) {
    state = watchReduce(state, { kind: "frame", frame: frame(start + 4) });
  }
  assert.equal(watchRows(state).length, WATCH_LINE_LIMIT);
  assert.equal(state.confirmed[0]?.text, line(100));
  assert.equal(
    new Set(state.confirmed.map((row) => row.id)).size,
    WATCH_LINE_LIMIT,
  );
  state = watchReduce(state, {
    kind: "seed",
    text: Array.from(
      { length: 2000 },
      (_, i) => `${line(i)}${"é".repeat(1000)}`,
    ).join("\n"),
  });
  assert.ok(watchBytes(state) <= WATCH_BYTE_LIMIT);
  assert.ok(watchRows(state).length <= WATCH_LINE_LIMIT);
  assert.equal(state.confirmed.at(-1)?.text, line(5099));
});

test("a seed arriving after watched rows needs a proven sequence relation before it can contribute", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  state = watchReduce(state, {
    kind: "seed",
    text: [line(0), line(1), line(2)].join("\n"),
  });
  assert.deepEqual(
    watchRows(state).map((row) => row.text),
    [line(0), line(1)],
  );
  let proven = watchReduce(baseline(), { kind: "frame", frame: frame(4) });
  proven = watchReduce(proven, {
    kind: "seed",
    text: [
      "earlier passive output",
      ...Array.from({ length: 4 }, (_, i) => line(i)),
    ].join("\n"),
  });
  assert.deepEqual(
    watchRows(proven).map((row) => row.text),
    ["earlier passive output", ...Array.from({ length: 4 }, (_, i) => line(i))],
  );
});

test("encoded byte pressure alone bounds passive seed text", () => {
  const state = watchReduce(baseline({ ...frame(), cols: 512 }), {
    kind: "seed",
    text: Array.from({ length: 2200 }, (_, i) => line(i).padEnd(510, "x")).join(
      "\n",
    ),
  });
  assert.ok(state.seed.length < 2200);
  assert.ok(watchBytes(state) <= WATCH_BYTE_LIMIT);
  assert.equal(state.seed.at(-1)?.text, line(2199).padEnd(510, "x"));
});

test("logical wrapped seed suffix is omitted instead of guessed into visible physical rows", () => {
  let state = baseline({ ...frame(), cols: 40 });
  state = watchReduce(state, {
    kind: "seed",
    text: [
      "Earlier passive output",
      "A logical seed line that is much longer than forty columns and wraps on the screen",
      "Another ambiguous suffix",
    ].join("\n"),
  });
  assert.deepEqual(
    state.seed.map((row) => row.text),
    ["Earlier passive output"],
  );
});

for (const scenario of recorded.cases) {
  test(`recorded default agent grid ${scenario.name}`, () => {
    let state = emptyWatch();
    for (const update of scenario.updates) {
      const { cols, rows, buffer } = update.grid;
      if (buffer !== "normal" && buffer !== "alternate")
        throw new Error("Invalid recorded buffer");
      const frame: WatchFrame = { cols, rows, buffer };
      if (update.kind === "reset") {
        state = watchReduce(state, {
          kind: "baseline",
          frame,
          continuity: scenario.agent,
          at: 1000,
        });
      } else if (update.kind === "data") {
        state = watchReduce(state, { kind: "frame", frame });
      } else throw new Error("Invalid recorded update");
    }
    assert.deepEqual(
      watchRows(state).map((row) => row.text),
      scenario.expectedDeparted,
      `${scenario.agent} native sequences ${scenario.nativeSeq.join(" → ")}`,
    );
  });
}

function proseFrame(seq: number): WatchFrame {
  const captured = prose.frames.find((item) => item.seq === seq);
  assert.ok(captured, `missing native sequence ${seq}`);
  assert.equal(captured.buffer, "normal");
  return { cols: captured.cols, buffer: "normal", rows: captured.rows };
}

function numberedRows(state: ReturnType<typeof emptyWatch>) {
  return watchRows(state).flatMap((row) => {
    const match = /^\s*(\d+)\.\s/.exec(row.text);
    return match ? [{ number: Number(match[1]), row }] : [];
  });
}

test("recorded Codex viewport reversal reuses departed occurrences at every stream step", () => {
  let state = baseline(proseFrame(618));
  let first119: number | undefined;
  for (let seq = 619; seq <= 623; seq++) {
    state = watchReduce(state, { kind: "frame", frame: proseFrame(seq) });
    const rows = numberedRows(state);
    assert.equal(
      rows.length,
      new Set(rows.map((item) => item.number)).size,
      `native sequence ${seq} duplicated a numbered occurrence`,
    );
    const current119 = rows.find((item) => item.number === 119)?.row.id;
    if (first119 === undefined) first119 = current119;
    else assert.equal(current119, first119, "revisit retains the row anchor");
  }
  assert.deepEqual(
    numberedRows(state).map((item) => item.number),
    [119, 120],
  );
});

test("recorded streaming Codex prose retains recoverable rows through rejected and partial paints", () => {
  let state = baseline(proseFrame(600));
  for (let seq = 601; seq <= 679; seq++) {
    state = watchReduce(state, { kind: "frame", frame: proseFrame(seq) });
    const rows = numberedRows(state);
    assert.equal(
      rows.length,
      new Set(rows.map((item) => item.number)).size,
      `native sequence ${seq} duplicated a numbered occurrence`,
    );
  }
  const rows = numberedRows(state);
  const truth = prose.provenance.truth.text.split("\n");
  for (const number of [129, 130, 131, 132]) {
    const captured = rows.find((item) => item.number === number);
    assert.ok(captured, `recoverable prose row ${number} was silently lost`);
    assert.ok(truth[number - 1]?.startsWith(captured.row.text.trim()));
    assert.ok(
      captured.row.text.trim().length > 40,
      "partial candidates must mature",
    );
  }
  assert.ok(
    rows.length >= 18,
    "replay must retain observed prose rather than only gaps",
  );
  assert.deepEqual(
    rows.map((item) => item.number),
    [...rows.map((item) => item.number)].sort((a, b) => a - b),
  );
});

test("an unrecoverable boundary in recorded Codex prose is visible before later output", () => {
  let state = baseline(proseFrame(618));
  state = watchReduce(state, { kind: "frame", frame: proseFrame(619) });
  for (let seq = 668; seq <= 679; seq++)
    state = watchReduce(state, { kind: "frame", frame: proseFrame(seq) });
  const rows = watchRows(state);
  const before = rows.findIndex((row) => /^\s*119\./.test(row.text));
  const gap = rows.findIndex((row) => row.text === "… some lines missed");
  const after = rows.findIndex((row) => /^\s*133\./.test(row.text));
  assert.ok(before >= 0 && after > before, "retain proven rows on each side");
  assert.ok(
    gap > before && gap < after,
    "unrecoverable boundary needs a visible gap",
  );
});

test("detach and reconnect preserve occurrence identity when the same viewport is uniquely anchored", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  const rows = watchRows(state);
  state = watchReduce(state, { kind: "detach" });
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 2000,
    frame: frame(),
  });
  state = watchReduce(state, { kind: "frame", frame: frame(2) });
  const resumed = watchRows(state);
  assert.deepEqual(resumed.slice(0, rows.length), rows);
  assert.equal(new Set(resumed.map((row) => row.id)).size, resumed.length);
  for (const row of rows)
    assert.equal(resumed.filter((item) => item.text === row.text).length, 1);
});

test("identical stdout rows at two proven forward positions retain separate occurrences", () => {
  const repeated =
    "A genuine repeated stdout message with the same exact wording";
  const repeatedFrame = (start: number) => {
    const current = frame(start);
    return {
      ...current,
      rows: current.rows.map((text) =>
        text === line(1) || text === line(14) ? repeated : text,
      ),
    };
  };
  let state = baseline(repeatedFrame(0));
  for (let start = 2; start <= 18; start += 2)
    state = watchReduce(state, { kind: "frame", frame: repeatedFrame(start) });
  const rows = watchRows(state).filter((row) => row.text === repeated);
  assert.equal(rows.length, 2);
  assert.notEqual(rows[0]?.id, rows[1]?.id);
});

function capturedParagraph(
  state: ReturnType<typeof emptyWatch>,
  number: number,
) {
  const lines = watchRows(state).map((row) =>
    row.text.trim().replace(/^• /, ""),
  );
  const start = lines.findIndex((text) => text.startsWith(`${number}. `));
  assert.ok(start >= 0, `missing paragraph ${number}`);
  let end = start + 1;
  while (
    end < lines.length &&
    !/^\d+\.\s/.test(lines[end] ?? "") &&
    lines[end] !== "… some lines missed"
  )
    end++;
  return lines.slice(start, end).filter(Boolean).join(" ");
}

test("recorded prose preserves complete paragraphs and every continuation occurrence", () => {
  let state = baseline(proseFrame(600));
  for (let seq = 601; seq <= 679; seq++)
    state = watchReduce(state, { kind: "frame", frame: proseFrame(seq) });
  const truth = prose.provenance.truth.text.split("\n");
  for (let number = 123; number <= 133; number++)
    assert.equal(capturedParagraph(state, number), truth[number - 1]);
});

test("recorded final paragraph spacing and backward navigation retain the complete observed tail", () => {
  let state = baseline(proseFrame(718));
  for (let seq = 719; seq <= 743; seq++) {
    const captured = proseFrame(seq);
    state = watchReduce(state, {
      kind: "frame",
      frame: {
        ...captured,
        rows: captured.rows.map((text) => text.padEnd(captured.cols)),
      },
    });
  }
  const truth = prose.provenance.truth.text.split("\n");
  for (let number = 146; number < 150; number++)
    assert.equal(capturedParagraph(state, number), truth[number - 1]);
  assert.ok(
    capturedParagraph(state, 150).startsWith(truth[149] ?? "missing truth"),
  );
  const numbers = numberedRows(state).map((item) => item.number);
  assert.equal(new Set(numbers).size, numbers.length);
});

test("a newly fixed header cannot overwrite early established prose occurrences", () => {
  let state = baseline(proseFrame(38));
  for (let seq = 39; seq <= 90; seq++)
    state = watchReduce(state, { kind: "frame", frame: proseFrame(seq) });
  const truth = prose.provenance.truth.text.split("\n");
  for (const number of [2, 3])
    assert.equal(capturedParagraph(state, number), truth[number - 1]);
});

test("seed and watched occurrences reconcile through the full observed ledger with stable prefix IDs", () => {
  let state = baseline(frame(10));
  const seed = (start: number, count: number) =>
    Array.from({ length: count }, (_, i) => line(start + i)).join("\n");
  state = watchReduce(state, { kind: "seed", text: seed(0, 16) });
  const previous = state.seed;
  state = watchReduce(state, { kind: "frame", frame: frame(12) });
  state = watchReduce(state, { kind: "seed", text: seed(-5, 24) });
  assert.deepEqual(
    watchRows(state).map((row) => row.text),
    Array.from({ length: 17 }, (_, i) => line(i - 5)),
  );
  assert.deepEqual(state.seed.slice(5), previous);
});

test("disconnected observed segments reuse their original occurrences on return", () => {
  let state = baseline();
  let original: ReturnType<typeof watchRows> = [];
  for (const start of [2, 4, 6, 40, 42, 44, 46, 0, 2, 4, 6]) {
    state = watchReduce(state, { kind: "frame", frame: frame(start) });
    if (start === 6 && !original.length) original = watchRows(state);
  }
  const rows = watchRows(state).filter((row) => row.kind === "text");
  assert.deepEqual(
    rows.filter((row) => original.some((known) => known.text === row.text)),
    original,
  );
  assert.equal(new Set(rows.map((row) => row.text)).size, rows.length);
  for (let i = 40; i < 46; i++)
    assert.ok(rows.some((row) => row.text === line(i)));
  assert.ok(watchRows(state).some((row) => row.kind === "gap"));
});

test("multiple established sequence alignments show a gap and withhold a guessed occurrence", () => {
  const repeatedFrame = (start: number): WatchFrame => ({
    ...frame(start),
    rows: [
      "Fixed header",
      ...Array.from({ length: 12 }, (_, i) => {
        const position = start + i;
        if (
          (position >= 5 && position < 17) ||
          (position >= 25 && position < 37)
        )
          return `Repeated block row ${(position - 5) % 20} with informative evidence`;
        return line(position);
      }),
      "Fixed footer",
    ],
  });
  let state = baseline(repeatedFrame(0));
  for (let start = 2; start <= 42; start += 2)
    state = watchReduce(state, { kind: "frame", frame: repeatedFrame(start) });
  const rows = watchRows(state).filter((row) => row.kind === "text");
  assert.deepEqual(
    rows.map((row) => row.text),
    Array.from({ length: 42 }, (_, i) => repeatedFrame(i).rows[1]),
  );
  assert.equal(watchRows(state).filter((row) => row.kind === "gap").length, 0);
  for (let i = 5; i < 17; i++) {
    assert.equal(rows[i]?.text, rows[i + 20]?.text);
    assert.notEqual(rows[i]?.id, rows[i + 20]?.id);
  }
  state = watchReduce(state, { kind: "frame", frame: repeatedFrame(5) });
  state = watchReduce(state, { kind: "frame", frame: repeatedFrame(7) });
  assert.deepEqual(
    watchRows(state).filter((row) => row.kind === "text"),
    rows,
  );
  assert.equal(watchRows(state).at(-1)?.kind, "gap");
});

test("unresolved redraw alternatives stay bounded and become a visible gap at departure", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  for (let i = 0; i < 100; i++) {
    const current = frame(2);
    state = watchReduce(state, {
      kind: "frame",
      frame: {
        ...current,
        rows: current.rows.map((text, y) =>
          y === 8
            ? `Unresolved animation state ${i} with different wording`
            : text,
        ),
      },
    });
  }
  if (state.ledger.kind !== "located")
    assert.fail("expected located occurrences");
  assert.ok(
    state.ledger.cells.every(
      (cell) => cell.kind !== "conflicted" || cell.alternatives.length <= 8,
    ),
  );
  for (let start = 4; start <= 12; start += 2)
    state = watchReduce(state, { kind: "frame", frame: frame(start) });
  assert.ok(watchRows(state).some((row) => row.kind === "gap"));
  assert.ok(
    !watchRows(state).some((row) =>
      row.text.startsWith("Unresolved animation"),
    ),
  );
});

test("an interrupted sample keeps known occurrence references for a later uniquely anchored revisit", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  const rows = watchRows(state);
  state = watchReduce(state, {
    kind: "frame",
    frame: frame(),
    interrupted: true,
  });
  state = watchReduce(state, { kind: "frame", frame: frame(2) });
  assert.deepEqual(watchRows(state), rows);
});

test("reopen maps the viewport without publishing a closed interval or replaying known rows", () => {
  const row = (index: number) =>
    `  Watched output ${String(index).padStart(5, "0")} has unique scrolling evidence`;
  const grid = (start: number): WatchFrame => ({
    cols: 96,
    buffer: "normal",
    rows: [
      "PINNED WATCH HEADER",
      ...Array.from({ length: 254 }, (_, i) => row(start + i)),
      "PINNED WATCH FOOTER",
    ],
  });
  let state = watchReduce(baseline(grid(0)), {
    kind: "frame",
    frame: grid(100),
  });
  const original = watchRows(state);
  assert.equal(original.length, 100);
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 2000,
    frame: grid(500),
  });
  assert.deepEqual(watchRows(state), original);
  state = watchReduce(state, { kind: "frame", frame: grid(600) });
  assert.equal(
    watchRows(state).filter((item) => item.kind === "text").length,
    200,
  );
  assert.equal(
    watchRows(state).filter((item) => item.kind === "gap").length,
    1,
  );
  const beforeClose = watchRows(state);
  state = watchReduce(state, {
    kind: "frame",
    frame: grid(700),
    interrupted: true,
  });
  assert.deepEqual(watchRows(state), beforeClose);
  state = watchReduce(state, { kind: "detach" });
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 3000,
    frame: grid(0),
  });
  assert.equal(
    watchRows(state).filter((item) => item.kind === "text").length,
    200,
    "reopen cannot publish stale pending rows",
  );
  assert.deepEqual(watchRows(state), beforeClose);
  state = watchReduce(state, { kind: "frame", frame: grid(100) });
  assert.deepEqual(
    watchRows(state),
    beforeClose,
    "known replay cannot append occurrences",
  );
  for (let start = 200; start <= 3000; start += 100) {
    state = watchReduce(state, { kind: "frame", frame: grid(start) });
    const rows = watchRows(state).filter((item) => item.kind === "text");
    assert.equal(
      new Set(rows.map((item) => item.text)).size,
      rows.length,
      `replay at ${start}`,
    );
  }
  const rows = watchRows(state).filter((item) => item.kind === "text");
  assert.equal(rows.length, 3000);
  assert.deepEqual(
    new Set(rows.map((item) => item.text)),
    new Set(Array.from({ length: 3000 }, (_, i) => row(i))),
  );
  assert.deepEqual(rows.slice(0, original.length), original);
  for (const known of beforeClose.filter((item) => item.kind === "text"))
    assert.equal(rows.find((item) => item.text === known.text)?.id, known.id);
});

test("disconnected candidate segments share the byte budget without replacing retained IDs", () => {
  const wideFrame = (start: number): WatchFrame => {
    const current = frame(start);
    return {
      ...current,
      cols: 4000,
      rows: current.rows.map((text) => text.padEnd(3000, "x")),
    };
  };
  let state = baseline(wideFrame(0));
  state = watchReduce(state, { kind: "frame", frame: wideFrame(2) });
  const original = watchRows(state);
  for (let i = 1; i <= 50; i++) {
    state = watchReduce(state, { kind: "frame", frame: wideFrame(i * 100) });
    state = watchReduce(state, {
      kind: "frame",
      frame: wideFrame(i * 100 + 2),
    });
    assert.ok(watchBytes(state) <= WATCH_BYTE_LIMIT);
  }
  assert.deepEqual(watchRows(state).slice(0, original.length), original);
  const returnTo = (start: number) => {
    const known = new Map(
      watchRows(state)
        .filter((row) => row.kind === "text")
        .map((row) => [row.text, row.id]),
    );
    for (const offset of [start + 1, start + 3, start, start + 2]) {
      state = watchReduce(state, { kind: "frame", frame: wideFrame(offset) });
      assert.ok(watchBytes(state) <= WATCH_BYTE_LIMIT);
    }
    const rows = watchRows(state).filter((row) => row.kind === "text");
    assert.equal(
      new Set(rows.map((row) => row.text)).size,
      rows.length,
      "return cannot repeat retained occurrences",
    );
    for (const row of rows)
      if (known.has(row.text)) assert.equal(row.id, known.get(row.text));
  };
  returnTo(0);
  assert.deepEqual(watchRows(state).slice(0, original.length), original);
  for (let i = 51; i <= 150; i++) {
    state = watchReduce(state, { kind: "frame", frame: wideFrame(i * 100) });
    state = watchReduce(state, {
      kind: "frame",
      frame: wideFrame(i * 100 + 2),
    });
    assert.ok(watchBytes(state) <= WATCH_BYTE_LIMIT);
  }
  const retained = watchRows(state).find((row) => row.kind === "text");
  assert.ok(retained);
  const match = /Unique content row (\d+)/.exec(retained.text);
  assert.ok(match);
  returnTo(Number(match[1]));
  assert.equal(
    new Set(watchRows(state).map((row) => row.id)).size,
    watchRows(state).length,
  );
});
