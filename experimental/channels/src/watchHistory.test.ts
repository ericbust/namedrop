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
    [line(0), line(1), line(70), line(71)],
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

test("identical text at different confirmed occurrences keeps distinct row IDs", () => {
  let state = watchReduce(baseline(), { kind: "frame", frame: frame(2) });
  state = watchReduce(state, {
    kind: "baseline",
    continuity: "pane-one",
    at: 2000,
    frame: frame(),
  });
  state = watchReduce(state, { kind: "frame", frame: frame(2) });
  assert.deepEqual(
    state.confirmed.map((row) => row.text),
    [line(0), line(1), line(0), line(1)],
  );
  assert.equal(new Set(state.confirmed.map((row) => row.id)).size, 4);
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
