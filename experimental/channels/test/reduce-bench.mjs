import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const options = Object.fromEntries(
  process.argv.slice(2).reduce((pairs, value, index, args) => {
    if (value.startsWith("--")) pairs.push([value.slice(2), args[index + 1]]);
    return pairs;
  }, []),
);
assert.ok(
  options.before && options.after && options.out,
  "Use --before <src> --after <src> --out <json>",
);
const rounds = Number(options.rounds ?? 5);
assert.ok(Number.isInteger(rounds) && rounds > 0);
const sampleCount = Number(options.samples ?? 60);
assert.ok(Number.isInteger(sampleCount) && sampleCount > 0);
const counts = options.profile ? [40_000] : [150, 2_000, 10_000, 40_000];
const key = "a".repeat(20);
const id = (n) => `m-${String(n).padStart(9, "0")}`;
function row(n) {
  return {
    message_id: id(n),
    thread_id: `thread-${Math.floor(n / 4)}`,
    in_reply_to: n % 4 === 0 ? null : id(n - (n % 4)),
    at_ms: 1_000_000 + n,
    author: "agent",
    kind: n % 4 === 0 ? "mention" : "reply",
    body: {
      text: `Message ${n} ${"shared-host benchmark body ".repeat(5)}`,
      truncated: false,
    },
    legs: [{ to: "peer", stage: { stage: "delivered" } }],
  };
}
function frame(rows) {
  return {
    key,
    frame: {
      wire: 1,
      space: {
        key,
        workspace_id: "w1",
        display_name: "bench",
        addressable_label: "bench",
      },
      health: { level: "calm" },
      participants: [],
      messages: rows,
      threads: [],
      attention: [],
    },
  };
}
async function load(path) {
  const directory = resolve(path);
  const sourceHashes = Object.fromEntries(
    await Promise.all(
      ["store.ts", "model.ts", "wire.ts"].map(async (name) => [
        name,
        createHash("sha256")
          .update(await readFile(resolve(directory, name)))
          .digest("hex"),
      ]),
    ),
  );
  const store = await import(pathToFileURL(resolve(directory, "store.ts")));
  const wire = await import(pathToFileURL(resolve(directory, "wire.ts")));
  return { ...store, ...wire, sourceHashes };
}
function sample(api, count) {
  const stored = Array.from({ length: count }, (_, n) => row(n));
  const first = api.parseEvent(
    "space",
    JSON.stringify(frame(stored.slice(-150))),
  );
  assert.equal(first?.type, "space");
  let state = api.reduce(api.initialState(), first);
  const channel = state.channels.get(key);
  assert.ok(channel);
  const messages = new Map(
    api
      .parseHistory([...stored].reverse())
      .map((message) => [message.id, message]),
  );
  state = {
    ...state,
    channels: new Map([
      [key, { ...channel, messages, runs: [[...messages.keys()]] }],
    ]),
  };
  const warmups = 12,
    trials = sampleCount;
  const events = Array.from({ length: warmups + trials }, (_, step) =>
    api.parseEvent(
      "space",
      JSON.stringify(
        frame(
          Array.from({ length: 150 }, (_, offset) =>
            row(count + step - 149 + offset),
          ),
        ),
      ),
    ),
  );
  const timings = [];
  const loadedBeforeTiming = [];
  let coldApplyMs = 0,
    failures = 0;
  for (const [index, event] of events.entries()) {
    assert.equal(event?.type, "space");
    if (index >= warmups)
      loadedBeforeTiming.push(state.channels.get(key).messages.size);
    const started = performance.now();
    state = api.reduce(state, event);
    const elapsed = performance.now() - started;
    if (index === 0) coldApplyMs = elapsed;
    if (index >= warmups) timings.push(elapsed);
    const current = state.channels.get(key),
      last = event.channel.messages.get(id(count + index));
    if (!last || current?.messages.get(last.id)?.body.text !== last.body.text)
      failures++;
    if (current?.runs.flat().length !== current?.messages.size) failures++;
    const retained = Math.min(count, api.HISTORY_CAP ?? count);
    if (current.messages.size > retained) {
      const order = current.runs.flat().slice(-retained);
      state = {
        ...state,
        channels: new Map([
          [
            key,
            {
              ...current,
              messages: new Map(
                order.map((id) => [id, current.messages.get(id)]),
              ),
              runs: [order],
            },
          ],
        ]),
      };
    }
  }
  assert.equal(
    failures,
    0,
    "Every timed frame must install its newest message and keep unique ordered coverage",
  );
  timings.sort((a, b) => a - b);
  return {
    seeded: count,
    retained: state.channels.get(key).messages.size,
    timedLoadedRange: [
      Math.min(...loadedBeforeTiming),
      Math.max(...loadedBeforeTiming),
    ],
    frames: trials,
    failures,
    coldApplyMs,
    p50Ms: timings[Math.floor(timings.length / 2)],
    minMs: timings[0],
    maxMs: timings.at(-1),
  };
}
const apis = {
  before: await load(options.before),
  after: await load(options.after),
};
const runs = [];
for (let round = 0; round < rounds; round++)
  for (const count of counts)
    for (const side of round % 2 === 0
      ? ["before", "after"]
      : ["after", "before"]) {
      const result = { round: round + 1, side, ...sample(apis[side], count) };
      runs.push(result);
      process.stdout.write(`${JSON.stringify(result)}\n`);
    }
const summary = counts.map((seeded) => {
  const cell = (side) => {
    const group = runs.filter(
        (run) => run.seeded === seeded && run.side === side,
      ),
      samples = group.map((run) => run.p50Ms).sort((a, b) => a - b);
    return {
      p50MedianMs: samples[Math.floor(samples.length / 2)],
      rangeMs: [samples[0], samples.at(-1)],
      retainedRange: [
        Math.min(...group.map((run) => run.retained)),
        Math.max(...group.map((run) => run.retained)),
      ],
      frames: group.reduce((sum, run) => sum + run.frames, 0),
      failures: group.reduce((sum, run) => sum + run.failures, 0),
    };
  };
  return { seeded, before: cell("before"), after: cell("after") };
});
await mkdir(resolve(options.out, ".."), { recursive: true });
await writeFile(
  options.out,
  `${JSON.stringify({ sources: { before: apis.before.sourceHashes, after: apis.after.sourceHashes }, node: process.version, rounds, warmupsPerRun: 12, samplesPerRun: sampleCount, description: "Preload requested history outside timing; time 150-row live windows appending one verified message each. Normalize loaded count to min(requested,HISTORY_CAP) between frames outside timing. Head cap applies during warmup and actual timed cache sizes are explicit. Parsing, fixture normalization and correctness checks are outside reduce timing.", summary, runs }, null, 2)}\n`,
);
if (options["max-p50-ms"]) {
  const result = summary.find((row) => row.seeded === 10_000);
  assert.ok(result, "The budget requires the 10k preload case");
  assert.ok(
    result.after.p50MedianMs < Number(options["max-p50-ms"]),
    `10k preload reduce p50 ${result.after.p50MedianMs.toFixed(3)} ms exceeds ${options["max-p50-ms"]} ms`,
  );
}
