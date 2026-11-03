import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { type TestContext, test } from "node:test";
import { type LiveState, type LiveUpdate, observePeek } from "./livePeek.ts";
import { peekTarget } from "./peek.ts";
import { parseEvent } from "./wire.ts";

const fixture = (
  await readFile(
    new URL("./fixtures/real-frames.jsonl", import.meta.url),
    "utf8",
  )
)
  .trim()
  .split("\n")[0];
assert.ok(fixture);
const event = parseEvent("space", fixture);
assert.equal(event?.type, "space");
if (event?.type !== "space") throw new Error("Invalid test fixture");
const member = [...event.channel.members.values()][0];
assert.ok(member);
const target = peekTarget(event.channel.key, member);
if (!target) throw new Error("Fixture must contain a Peek target");
const selected = target;
const encoded = (text: string) => Buffer.from(text).toString("base64");
const reset = (extra = {}) => ({
  v: 1,
  seq: 1,
  kind: "reset",
  continuity: "fixture-continuity",
  cols: 120,
  rows: 40,
  bytes: encoded("\x1b[HRED"),
  ...extra,
});
const data = (extra = {}) => ({
  v: 1,
  seq: 2,
  kind: "data",
  bytes: encoded("\x1b[HNEW"),
  ...extra,
});
const closed = (extra = {}) => ({
  v: 1,
  seq: 3,
  kind: "closed",
  reason: "ended",
  ...extra,
});
function response(records: unknown[], { fragment = 0, ended = true } = {}) {
  const raw = new TextEncoder().encode(
    records
      .map((r) => (typeof r === "string" ? r : `${JSON.stringify(r)}\n`))
      .join(""),
  );
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        for (let i = 0; i < raw.length; i += fragment || raw.length)
          controller.enqueue(raw.slice(i, i + (fragment || raw.length)));
        if (ended) controller.close();
      },
    }),
    { headers: { "Content-Type": "application/x-ndjson" } },
  );
}
function observation(
  t: TestContext,
  fetcher: typeof fetch,
  apply?: (update: LiveUpdate) => Promise<void>,
) {
  t.mock.method(globalThis, "fetch", fetcher);
  const controller = new AbortController();
  t.after(() => controller.abort());
  const states: LiveState[] = [];
  const updates: LiveUpdate[] = [];
  const done = observePeek({
    target: selected,
    signal: controller.signal,
    onState: (s) => states.push(s),
    async apply(update) {
      updates.push(update);
      await apply?.(update);
    },
  });
  return { controller, states, updates, done };
}
async function flush() {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

test("live boundary consumes fragmented NDJSON, heartbeats and atomic paints in order", async (t) => {
  const seen: { input: string; init?: RequestInit }[] = [];
  const run = observation(t, async (input, init) => {
    seen.push({ input: String(input), init });
    return response(
      [
        "\n\r\n",
        reset(),
        data(),
        reset({ seq: 3, cols: 40, rows: 10 }),
        closed({ seq: 4 }),
      ],
      { fragment: 1 },
    );
  });
  await run.done;
  assert.deepEqual(run.states, [
    { kind: "connecting" },
    { kind: "live" },
    { kind: "live" },
    { kind: "closed", reason: "ended" },
  ]);
  assert.deepEqual(
    run.updates.map((u) => u.kind),
    ["reset", "data", "reset"],
  );
  assert.deepEqual(run.updates[0], {
    kind: "reset",
    viewport: { cols: 120, rows: 40 },
    bytes: new TextEncoder().encode("\x1b[HRED"),
  });
  assert.equal(
    seen[0]?.input,
    `/w/${selected.channel}/participants/${encodeURIComponent(selected.member)}/live`,
  );
  assert.equal(seen[0]?.init?.credentials, "same-origin");
  assert.equal(seen[0]?.init?.cache, "no-store");
  assert.equal(seen[0]?.init?.redirect, "error");
});

test("history snapshots require a reset and obey exact wire and size bounds", async (t) => {
  const history = (seq: number, text: string, extra = {}) => ({
    v: 1,
    seq,
    kind: "history",
    text,
    ...extra,
  });
  await t.test("valid snapshots", async (subtest) => {
    const valid = observation(subtest, async () =>
      response([
        reset(),
        history(2, "first\nsecond"),
        history(3, "new window"),
        closed({ seq: 4 }),
      ]),
    );
    await valid.done;
    assert.deepEqual(
      valid.updates.map((update) => update.kind),
      ["reset", "history", "history"],
    );
    assert.deepEqual(valid.updates[1], {
      kind: "history",
      text: "first\nsecond",
    });
  });
  for (const records of [
    [history(1, "early")],
    [reset(), history(2, "extra", { extra: true })],
    [reset(), { ...history(2, "valid"), text: 12 }],
    [reset(), history(2, "é".repeat(524289))],
    [reset(), history(2, "line\n".repeat(2200))],
  ]) {
    await t.test("reject invalid snapshot", async (subtest) => {
      const run = observation(subtest, async () => response(records));
      await run.done;
      assert.deepEqual(run.states.at(-1), {
        kind: "unavailable",
        reason: "invalid_source",
      });
    });
  }
});

test("live boundary rejects malformed/version/sequence/base64/viewport/variant bytes before rendering them", async (t) => {
  const invalid = [
    reset({ v: 2 }),
    reset({ seq: 0 }),
    reset({ seq: 2 }),
    reset({ epoch: 1 }),
    reset({ cols: 0 }),
    reset({ cols: 513 }),
    reset({ rows: 257 }),
    reset({ cols: 512, rows: 256 }),
    reset({ cols: 1.5 }),
    reset({ continuity: "" }),
    reset({ continuity: "a&b" }),
    reset({ continuity: "x".repeat(257) }),
    reset({ bytes: "Zh==" }),
    reset({ bytes: "@@@@" }),
    reset({ bytes: "Zg" }),
    reset({ bytes: "Zg=== " }),
    reset({ bytes: Buffer.alloc(1024 * 1024 + 1).toString("base64") }),
    { v: 1, seq: 1, kind: "terminal.frame" },
    data({ seq: 1 }),
    { v: 1, seq: 1, kind: "closed", reason: "native secret reason" },
    { v: 1, seq: 1, kind: "unavailable", reason: "unknown" },
    "{broken}\n",
    "x".repeat(2 * 1024 * 1024 + 1),
  ];
  let next: unknown;
  t.mock.method(globalThis, "fetch", async () => response([next]));
  for (const record of invalid) {
    next = record;
    const states: LiveState[] = [],
      updates: LiveUpdate[] = [];
    await observePeek({
      target: selected,
      signal: new AbortController().signal,
      onState: (s) => states.push(s),
      async apply(u) {
        updates.push(u);
      },
    });
    assert.deepEqual(states.at(-1), {
      kind: "unavailable",
      reason: "invalid_source",
    });
    assert.deepEqual(updates, []);
  }
});

test("data is bounded and never accepted before the response's reset", async (t) => {
  const run = observation(t, async () =>
    response([
      reset(),
      data({ bytes: Buffer.alloc(65537).toString("base64") }),
    ]),
  );
  await run.done;
  assert.equal(run.updates.length, 1);
  assert.deepEqual(run.states.at(-1), {
    kind: "unavailable",
    reason: "invalid_source",
  });
});

test("retry keeps continuity and response seq restarts at one; a changed paint never reaches the renderer", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const urls: string[] = [];
  const run = observation(t, async (input) => {
    urls.push(String(input));
    return urls.length === 1
      ? response([reset()])
      : response([reset({ continuity: "replacement" })]);
  });
  await flush();
  assert.equal(urls.length, 1);
  assert.equal(run.states.at(-1)?.kind, "reconnecting");
  t.mock.timers.tick(1000);
  await run.done;
  assert.ok(urls[1]?.endsWith("?continuity=fixture-continuity"));
  assert.equal(run.updates.length, 1);
  assert.deepEqual(run.states.at(-1), { kind: "closed", reason: "replaced" });
});

test("eligible failures retry three times only, without resetting the budget after valid paints", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let calls = 0;
  const run = observation(t, async () => {
    calls++;
    return response([reset()]);
  });
  await flush();
  for (const delay of [1000, 2000, 4000]) {
    t.mock.timers.tick(delay);
    await flush();
  }
  await run.done;
  assert.equal(calls, 4);
  assert.deepEqual(run.states.at(-1), {
    kind: "unavailable",
    reason: "interrupted",
  });
});

test("HTTP identity/session failures stop without automatic retries", async (t) => {
  let status = 401;
  let calls = 0;
  t.mock.method(globalThis, "fetch", async () => {
    calls++;
    return new Response(null, { status });
  });
  for (const [code, expected] of [
    [401, { kind: "session-ended" }],
    [403, { kind: "session-ended" }],
    [404, { kind: "closed", reason: "missing" }],
    [410, { kind: "closed", reason: "replaced" }],
  ] as const) {
    status = code;
    const states: LiveState[] = [];
    await observePeek({
      target: selected,
      signal: new AbortController().signal,
      async apply() {
        assert.fail("No paint allowed");
      },
      onState: (s) => states.push(s),
    });
    assert.deepEqual(states.at(-1), expected);
  }
  assert.equal(calls, 4);
});

test("unsupported is a startup fallback signal only before continuity is accepted", async (t) => {
  let records: unknown[] = [
    { v: 1, seq: 1, kind: "unavailable", reason: "unsupported" },
  ];
  t.mock.method(globalThis, "fetch", async () => response(records));
  for (const established of [false, true]) {
    if (established)
      records = [
        reset(),
        { v: 1, seq: 2, kind: "unavailable", reason: "unsupported" },
      ];
    const states: LiveState[] = [];
    await observePeek({
      target: selected,
      signal: new AbortController().signal,
      async apply() {},
      onState: (s) => states.push(s),
    });
    assert.deepEqual(states.at(-1), {
      kind: "unavailable",
      reason: established ? "invalid_source" : "unsupported",
    });
  }
});

test("held renderer completion applies backpressure; abort settles without a disposed write callback", async (t) => {
  const run = observation(
    t,
    async () => response([reset(), data(), closed()], { ended: false }),
    () => new Promise(() => {}),
  );
  await flush();
  assert.equal(run.updates.length, 1);
  assert.deepEqual(run.states, [{ kind: "connecting" }]);
  run.controller.abort();
  await run.done;
  assert.equal(run.updates.length, 1);
});

test("watchdog cancels an otherwise unresolved reader and retry wait is abortable", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let canceled = 0;
  const run = observation(
    t,
    async () =>
      new Response(
        new ReadableStream({
          cancel() {
            canceled++;
          },
        }),
        { headers: { "Content-Type": "application/x-ndjson" } },
      ),
  );
  await flush();
  t.mock.timers.tick(30_000);
  await flush();
  assert.equal(canceled, 1);
  assert.equal(run.states.at(-1)?.kind, "reconnecting");
  run.controller.abort();
  await run.done;
});

test("server-owned canonical streams conform without copying or skipping the interface fixture", async (t) => {
  const path =
    process.env.LIVE_PEEK_FIXTURES ??
    new URL("../../../tests/fixtures/live_peek/streams.json", import.meta.url);
  const canonical = JSON.parse(await readFile(path, "utf8"));
  assert.equal(canonical.version, 1);
  assert.ok(Array.isArray(canonical.streams) && canonical.streams.length > 0);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let records: unknown[] = [];
  t.mock.method(globalThis, "fetch", async () => response(records));
  for (const stream of canonical.streams) {
    records = stream.records;
    assert.ok(Array.isArray(records) && records.length > 0, stream.name);
    const states: LiveState[] = [],
      updates: LiveUpdate[] = [];
    const done = observePeek({
      target: selected,
      signal: new AbortController().signal,
      onState: (s) => states.push(s),
      async apply(update) {
        updates.push(update);
      },
    });
    await flush();
    for (const delay of [1000, 2000, 4000]) {
      t.mock.timers.tick(delay);
      await flush();
    }
    await done;
    const last = stream.records.at(-1);
    const expected =
      last.kind === "closed"
        ? { kind: "closed", reason: last.reason }
        : {
            kind: "unavailable",
            reason: ["unsupported", "busy", "invalid_source"].includes(
              last.reason,
            )
              ? last.reason
              : "interrupted",
          };
    assert.deepEqual(states.at(-1), expected, stream.name);
    assert.equal(
      updates.length,
      stream.records.filter(
        (r: { kind: string }) => r.kind === "reset" || r.kind === "data",
      ).length,
      stream.name,
    );
  }
  records = [
    reset({
      bytes: Buffer.alloc(canonical.limits.decoded_paint).toString("base64"),
    }),
    data({
      bytes: Buffer.alloc(canonical.limits.decoded_data).toString("base64"),
    }),
    closed(),
  ];
  const states: LiveState[] = [];
  await observePeek({
    target: selected,
    signal: new AbortController().signal,
    async apply() {},
    onState: (s) => states.push(s),
  });
  assert.deepEqual(states.at(-1), { kind: "closed", reason: "ended" });
});
