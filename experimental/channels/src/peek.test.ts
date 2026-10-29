import assert from "node:assert/strict";
import { test } from "node:test";
import { peekCopy } from "./copy.ts";
import type { ScreenFailure, ScreenResult } from "./model.ts";
import {
  firstRead,
  type PeekRead,
  type PeekReadEvent,
  peekReduce,
  peekView,
} from "./peek.ts";
import type { PeekWho } from "./store.ts";

const who: PeekWho = {
  alias: "@codex",
  kind: "agent",
  activity: "working",
  channel: { label: "#alpha", suffix: null },
};
const early = new Date(2026, 9, 29, 22, 42).getTime();
const later = new Date(2026, 9, 29, 22, 43).getTime();
const clock = (ms: number) =>
  new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
const text = (body: string): ScreenResult => ({
  kind: "text",
  screen: { earlierOmitted: false, text: body },
});
const failed = (failure: ScreenFailure): ScreenResult => ({
  kind: "failed",
  failure,
});
function run(...events: PeekReadEvent[]): PeekRead {
  return events.reduce(peekReduce, firstRead());
}
const shownAt = (body: string, at = early) =>
  run({ type: "settled", request: 1, result: text(body), at });

test("reading table: a first read under 400 ms shows the header and an empty body", () => {
  assert.deepEqual(peekView(firstRead(), who), {
    stamp: null,
    refreshing: true,
    bar: null,
    body: { kind: "blank" },
  });
});
test("reading table: a first read over 400 ms names the screen, and over 2 s adds the busy line", () => {
  const slow = run({ type: "waited", request: 1, wait: "slow" });
  assert.deepEqual(peekView(slow, who).body, { kind: "reading", busy: false });
  assert.equal(peekView(slow, who).stamp, null);
  const busy = peekReduce(slow, { type: "waited", request: 1, wait: "busy" });
  assert.deepEqual(peekView(busy, who).body, { kind: "reading", busy: true });
  assert.equal(
    peekReduce(busy, { type: "waited", request: 1, wait: "slow" }),
    busy,
    "a late slow tick never steps back",
  );
  assert.equal(peekCopy.readingScreen("@codex"), "Reading @codex's screen…");
  assert.equal(peekCopy.busy, "Busy panes can take a few seconds.");
});
test("reading table: a refresh keeps the earlier text and stamp, disables Refresh, and reads Reading… after 400 ms", () => {
  const shown = shownAt("› Reading README.md");
  const refreshing = peekReduce(shown, { type: "again" });
  assert.equal(refreshing.phase, "reading");
  assert.deepEqual(peekView(refreshing, who), {
    stamp: `Read at ${clock(early)}`,
    refreshing: true,
    bar: null,
    body: { kind: "text", earlierOmitted: false, text: "› Reading README.md" },
  });
  const slow = peekReduce(refreshing, {
    type: "waited",
    request: 2,
    wait: "slow",
  });
  assert.equal(peekView(slow, who).stamp, "Reading…");
  assert.deepEqual(peekView(slow, who).body, peekView(refreshing, who).body);
});
test("reading table: a finished read replaces the text and stamps the time", () => {
  const shown = shownAt("old");
  assert.deepEqual(peekView(shown, who), {
    stamp: `Read at ${clock(early)}`,
    refreshing: false,
    bar: null,
    body: { kind: "text", earlierOmitted: false, text: "old" },
  });
  const next = peekReduce(peekReduce(shown, { type: "again" }), {
    type: "settled",
    request: 2,
    result: text("new"),
    at: later,
  });
  assert.equal(peekView(next, who).stamp, `Read at ${clock(later)}`);
  assert.deepEqual(peekView(next, who).body, {
    kind: "text",
    earlierOmitted: false,
    text: "new",
  });
});
test("reading table: an empty or whitespace screen reads The screen is empty, and the omission marker is lifted out", () => {
  for (const body of ["", "\n", "  \t\n "])
    assert.deepEqual(peekView(shownAt(body), who).body, { kind: "empty" });
  assert.equal(peekCopy.empty, "The screen is empty.");
  const omitted = run({
    type: "settled",
    request: 1,
    result: { kind: "text", screen: { earlierOmitted: true, text: "tail" } },
    at: early,
  });
  assert.deepEqual(peekView(omitted, who).body, {
    kind: "text",
    earlierOmitted: true,
    text: "tail",
  });
});
test("one read at a time: Refresh during a read changes nothing, and a stale answer or tick never lands", () => {
  const reading = firstRead();
  assert.equal(peekReduce(reading, { type: "again" }), reading);
  const shown = shownAt("one");
  const second = peekReduce(shown, { type: "again" });
  assert.equal(second.request, 2);
  assert.equal(
    peekReduce(second, {
      type: "settled",
      request: 1,
      result: text("late"),
      at: later,
    }),
    second,
  );
  assert.equal(
    peekReduce(second, { type: "waited", request: 1, wait: "busy" }),
    second,
  );
  assert.equal(
    peekReduce(shown, {
      type: "settled",
      request: 1,
      result: text("again"),
      at: later,
    }),
    shown,
  );
});

const table: [ScreenFailure, string, string, boolean][] = [
  [
    "closed",
    "Pane closed",
    "@codex no longer has a pane Namedrop can read.",
    false,
  ],
  [
    "missing",
    "Not in #alpha any more",
    "Namedrop can't find @codex in this workspace.",
    false,
  ],
  [
    "busy",
    "Couldn't read the screen",
    "Herdr was busy or didn't answer in time.",
    true,
  ],
  [
    "session",
    "Session ended",
    "Open Namedrop's dashboard link again to keep reading.",
    false,
  ],
  [
    "unreachable",
    "Couldn't reach Namedrop",
    "Check that Namedrop is still running.",
    true,
  ],
];
test("failure table: with no earlier text the notice fills the body, with Try again only where a retry can help", () => {
  for (const [failure, title, body, retry] of table) {
    const read = run({
      type: "settled",
      request: 1,
      result: failed(failure),
      at: later,
    });
    assert.deepEqual(peekView(read, who), {
      stamp: null,
      refreshing: false,
      bar: null,
      body: { kind: "failure", notice: { title, body, retry } },
    });
  }
});
test("failure table: over earlier text the notice becomes the bar, and the text keeps its old stamp", () => {
  for (const [failure, title, body, retry] of table) {
    const read = peekReduce(peekReduce(shownAt("kept"), { type: "again" }), {
      type: "settled",
      request: 2,
      result: failed(failure),
      at: later,
    });
    assert.deepEqual(peekView(read, who), {
      stamp: `Read at ${clock(early)}`,
      refreshing: false,
      bar: { title, body, retry },
      body: { kind: "text", earlierOmitted: false, text: "kept" },
    });
    const retried = peekReduce(read, { type: "again" });
    assert.equal(
      retried.phase,
      "reading",
      "Try again reads exactly like Refresh",
    );
    assert.equal(peekView(retried, who).bar, null);
  }
});

test("a retained snapshot seeds a fresh read with its original timestamp", () => {
  const snapshot = {
    screen: { earlierOmitted: true, text: "retained tail" },
    at: early,
  };
  const read = firstRead(snapshot);
  assert.deepEqual(read, {
    phase: "reading",
    request: 1,
    wait: "brief",
    shown: snapshot,
  });
  assert.deepEqual(peekView(read, who), {
    stamp: `Read at ${clock(early)}`,
    refreshing: true,
    bar: null,
    body: { kind: "text", earlierOmitted: true, text: "retained tail" },
  });
  const settled = peekReduce(read, {
    type: "settled",
    request: 1,
    result: text("fresh"),
    at: later,
  });
  assert.equal(peekView(settled, who).stamp, `Read at ${clock(later)}`);
  assert.deepEqual(settled.shown, {
    screen: { earlierOmitted: false, text: "fresh" },
    at: later,
  });
});

test("a retained empty snapshot stays empty during a fresh read and failures keep it", () => {
  const snapshot = { screen: { earlierOmitted: false, text: "" }, at: early };
  const read = firstRead(snapshot);
  assert.deepEqual(peekView(read, who).body, { kind: "empty" });
  const failure = peekReduce(read, {
    type: "settled",
    request: 1,
    result: failed("busy"),
    at: later,
  });
  assert.equal(failure.shown, snapshot);
  assert.equal(peekView(failure, who).stamp, `Read at ${clock(early)}`);
  assert.deepEqual(peekView(failure, who).body, { kind: "empty" });
});

for (const failure of ["closed", "missing", "session"] as const) {
  test(`target or session failure ${failure} replaces earlier text and its timestamp`, () => {
    const snapshot = {
      screen: { earlierOmitted: false, text: "old screen" },
      at: early,
    };
    for (const reading of [
      firstRead(snapshot),
      peekReduce(shownAt("old screen"), { type: "again" }),
    ]) {
      const read = peekReduce(reading, {
        type: "settled",
        request: reading.request,
        result: failed(failure),
        at: later,
      });
      const view = peekView(read, who);
      assert.equal(
        view.body.kind,
        "failure",
        "invalid screen must be replaced by its failure",
      );
      assert.equal(
        view.stamp,
        null,
        "invalid screen must not retain its read time",
      );
      assert.equal(
        view.bar,
        null,
        "invalid screen must not stay behind a failure bar",
      );
    }
  });
}
