import assert from "node:assert/strict";
import { test } from "node:test";
import { type DirectPeer, heardDirect } from "./direct-messages.ts";
import type { AddressId, SpaceKey, State } from "./model.ts";
import {
  type AgentKey,
  agentKey,
  burstTone,
  createPlayer,
  nextRing,
  noRings,
  nothingHeard,
  type PlayerEnv,
  parseRingRecord,
  parseSoundSettings,
  RECIPES,
  type SoundSettings,
  saveOf,
  soundFor,
  TONES,
} from "./sounds.ts";
import { initialState, reduce } from "./store.ts";
import { parseEvent } from "./wire.ts";

const agent = (id: string) => id as AgentKey;
const settings = (
  value: Partial<{
    default: string;
    muted: boolean;
    agents: Record<string, string>;
  }> = {},
): SoundSettings => {
  const parsed = parseSoundSettings({
    default: "chime",
    muted: false,
    agents: {},
    ...value,
  });
  assert.ok(parsed, "settings fixture must parse");
  return parsed;
};

test("sounds: an agent rings its own choice, else the Default, and never when muted or None", () => {
  const chosen = settings({ agents: { "pane-address-1-2-3": "pop" } });
  assert.equal(soundFor(chosen, agent("pane-address-1-2-3")), "pop");
  assert.equal(soundFor(chosen, agent("pane-address-9-9-9")), "chime");
  assert.equal(
    soundFor(
      settings({ agents: { "pane-address-1-2-3": "none" } }),
      agent("pane-address-1-2-3"),
    ),
    null,
  );
  assert.equal(
    soundFor(settings({ default: "none" }), agent("pane-address-9-9-9")),
    null,
  );
  assert.equal(
    soundFor(
      settings({ muted: true, agents: { "pane-address-1-2-3": "pop" } }),
      agent("pane-address-1-2-3"),
    ),
    null,
  );
  assert.equal(soundFor(null, agent("pane-address-1-2-3")), null);
});

test("sounds: a save carries the value the page showed before it, and null for an agent on the Default", () => {
  const shown = settings({
    default: "bell",
    muted: true,
    agents: { "pane-address-1-2-3": "pop" },
  });
  assert.deepEqual(saveOf(shown, { kind: "default", sound: "ping" }), {
    kind: "default",
    sound: "ping",
    was: "bell",
  });
  assert.deepEqual(saveOf(shown, { kind: "muted", muted: false }), {
    kind: "muted",
    muted: false,
    was: true,
  });
  const claude = agent("pane-address-1-2-3");
  assert.deepEqual(
    saveOf(shown, { kind: "agent", agent: claude, sound: null }),
    {
      kind: "agent",
      agent: claude,
      sound: null,
      was: "pop",
    },
  );
  const codex = agent("pane-address-4-5-6");
  assert.deepEqual(
    saveOf(shown, { kind: "agent", agent: codex, sound: "soft" }),
    {
      kind: "agent",
      agent: codex,
      sound: "soft",
      was: null,
    },
  );
});

test("sounds: a burst rings the first arrival that has a sound", () => {
  const chosen = settings({
    agents: { a: "none", b: "knock", c: "bell" },
  });
  assert.equal(
    burstTone(chosen, [agent("a"), agent("b"), agent("c")]),
    "knock",
  );
  assert.equal(burstTone(chosen, [agent("a")]), null);
});

test("sounds: settings parse at the wire and drop what the contract refuses", () => {
  assert.deepEqual(
    parseSoundSettings({
      default: "bell",
      muted: true,
      agents: {
        "pane-address-1-2-3": "pop",
        "bad id!": "pop",
        "pane-address-4-5-6": "trumpet",
      },
    }),
    {
      default: "bell",
      muted: true,
      agents: new Map([["pane-address-1-2-3", "pop"]]),
    },
  );
  assert.equal(parseSoundSettings({ default: "trumpet", muted: false }), null);
  assert.equal(parseSoundSettings({ default: "chime" }), null);
  assert.equal(parseSoundSettings("chime"), null);
});

test("sounds: an envoy rings as the agent it stands for", () => {
  assert.equal(
    agentKey("envoy:w2:pane-address-1-2-3" as AddressId),
    "pane-address-1-2-3",
  );
  assert.equal(
    agentKey("pane-address-1-2-3" as AddressId),
    "pane-address-1-2-3",
  );
  assert.equal(agentKey("x".repeat(129) as AddressId), null);
});

test("sounds: the eight sounds are short and each starts on its own pitch", () => {
  assert.deepEqual([...TONES].sort(), [
    "bell",
    "blip",
    "chime",
    "knock",
    "marimba",
    "ping",
    "pop",
    "soft",
  ]);
  const firsts = TONES.map((tone) => RECIPES[tone][0]?.freq);
  assert.equal(new Set(firsts).size, 8);
  for (const tone of TONES) {
    const end = Math.max(...RECIPES[tone].map((note) => note.at + note.dur));
    assert.ok(end < 0.6, `${tone} lasts ${end}s`);
    for (const note of RECIPES[tone]) assert.ok(note.gain <= 1);
  }
});

const keyA = "a".repeat(20);
function participant(address_id: string, type = "agent") {
  return {
    address_id,
    alias: `@${address_id}`,
    kind: type === "agent" ? { type, runtime: "codex" } : { type },
    activity: "idle",
    role: null,
    pane_label: null,
  };
}
type Row = {
  id: string;
  author: string;
  to: string[];
  kind?: string;
  progress_note?: boolean;
  at?: number;
};
function frame(rows: Row[], key = keyA) {
  return {
    key,
    frame: {
      wire: 1,
      generated_at_ms: 100,
      space: {
        key,
        workspace_id: "w1",
        display_name: "alpha",
        addressable_label: "alpha",
      },
      health: { level: "calm" },
      participants: [
        participant("human", "human"),
        participant("you-too", "human"),
        participant("claude"),
        participant("codex"),
        participant("shell", "terminal"),
      ],
      messages: rows.map((row, n) => ({
        message_id: row.id,
        thread_id: row.id,
        in_reply_to: null,
        at_ms: row.at ?? 100 + n,
        author: row.author,
        kind: row.kind ?? "reply",
        progress_note: row.progress_note ?? false,
        body: { text: row.id, truncated: false },
        legs: row.to.map((to) => ({ to, stage: { stage: "delivered" } })),
      })),
      threads: [],
      attention: [],
    },
  };
}
function apply(state: State, rows: Row[], key = keyA): State {
  const event = parseEvent("space", JSON.stringify(frame(rows, key)));
  assert.ok(event, "frame must parse");
  return reduce(state, event);
}
function ready(state: State, keys: string[]): State {
  const event = parseEvent("ready", JSON.stringify({ keys }));
  assert.ok(event, "ready must parse");
  return reduce(state, event);
}
const keyB = "b".repeat(20);
const keyC = "c".repeat(20);
const before: Row = { id: "old", author: "claude", to: ["human"] };
const loaded = apply(initialState(), [before]);

test("sounds: the first load of a channel hears nothing, even an agent's message to the person", () => {
  assert.deepEqual(loaded.heard, nothingHeard);
});

test("sounds: an agent's new message to the person is heard once, by its author", () => {
  const next = apply(loaded, [
    before,
    { id: "answer", author: "claude", to: ["human", "codex"] },
  ]);
  assert.deepEqual(next.heard, {
    seq: 1,
    arrivals: [{ agent: "claude", id: "answer" }],
  });
  const again = apply(next, [
    before,
    { id: "answer", author: "claude", to: ["human", "codex"] },
  ]);
  assert.equal(again.heard, next.heard);
  const envoy = apply(next, [
    { id: "far", author: "envoy:w2:pane-address-7-8-9", to: ["you-too"] },
  ]);
  assert.deepEqual(envoy.heard, {
    seq: 2,
    arrivals: [{ agent: "pane-address-7-8-9", id: "far" }],
  });
});

test("sounds: agent-to-agent traffic, the person's own posts, terminals, system kinds and progress notes stay silent", () => {
  const quiet: Row[] = [
    { id: "a2a", author: "claude", to: ["codex"] },
    { id: "mine", author: "human", to: ["claude"] },
    { id: "mine-too", author: "you-too", to: ["claude", "human"] },
    { id: "run", author: "shell", to: ["human"] },
    { id: "reminder", author: "claude", to: ["human"], kind: "reminder" },
    { id: "nudge", author: "claude", to: ["human"], kind: "nudge" },
    { id: "hook", author: "claude", to: ["human"], kind: "hook" },
    { id: "note", author: "claude", to: ["human"], progress_note: true },
  ];
  assert.equal(apply(loaded, [before, ...quiet]).heard, loaded.heard);
});

test("sounds: a reconnect rings for messages to the person that arrived while it was down, but the first load does not", () => {
  let state = reduce(initialState(), { type: "opened" });
  state = ready(apply(state, [{ ...before, at: 500 }]), [keyA]);
  assert.equal(state.heard, nothingHeard);
  state = reduce(state, { type: "disconnected" });
  state = reduce(state, { type: "opened" });
  state = apply(state, [
    { ...before, at: 500 },
    { id: "aside", author: "claude", to: ["codex"], at: 600 },
    { id: "missed", author: "codex", to: ["human"], at: 601 },
  ]);
  assert.equal(state.heard, nothingHeard, "nothing rings mid-replay");
  state = ready(state, [keyA]);
  assert.equal(state.sync.kind, "live");
  assert.deepEqual(state.heard, {
    seq: 1,
    arrivals: [{ agent: "codex", id: "missed" }],
  });
});

test("sounds: a channel that first appears after the first load rings for its new messages to the person, not its old ones", () => {
  let state = reduce(initialState(), { type: "opened" });
  state = apply(state, [{ ...before, at: 500 }]);
  state = ready(state, [keyA]);
  assert.equal(state.heard, nothingHeard);
  state = apply(
    state,
    [{ id: "old", author: "claude", to: ["human"], at: 400 }],
    keyB,
  );
  assert.ok(state.channels.has(keyB as SpaceKey));
  assert.equal(state.heard, nothingHeard);
  state = apply(
    state,
    [
      { id: "aside", author: "claude", to: ["codex"], at: 600 },
      { id: "new", author: "codex", to: ["human"], at: 601 },
    ],
    keyC,
  );
  assert.deepEqual(state.heard, {
    seq: 1,
    arrivals: [{ agent: "codex", id: "new" }],
  });
});

test("sounds: a channel first seen later rings for a message newer than the page load, though older than live traffic since", () => {
  let state = reduce(initialState(), { type: "opened" });
  state = ready(apply(state, [{ ...before, at: 100 }]), [keyA]);
  state = apply(state, [
    { ...before, at: 100 },
    { id: "a2a", author: "claude", to: ["codex"], at: 900 },
  ]);
  state = apply(
    state,
    [{ id: "late", author: "codex", to: ["human"], at: 800 }],
    keyB,
  );
  assert.deepEqual(state.heard, {
    seq: 1,
    arrivals: [{ agent: "codex", id: "late" }],
  });
});

function peer(
  addressId: string,
  lastAgentMessageId: string | null,
): DirectPeer {
  return {
    spaceKey: keyA as SpaceKey,
    addressId: addressId as AddressId,
    alias: `@${addressId}`,
    workspaceLabel: "alpha",
    paneLabel: null,
    runtime: "codex",
    activity: "idle",
    closed: false,
    threadId: null,
    lastActivityMs: null,
    lastMessageId: lastAgentMessageId,
    lastAgentMessageId,
    attentionCount: 0,
  };
}

test("sounds: after the first read, a direct message is heard when an agent's last message changes or a new peer brings one", () => {
  const first = [peer("claude", "m1"), peer("codex", null)];
  assert.deepEqual(heardDirect(null, first), []);
  assert.deepEqual(heardDirect(first, first), []);
  assert.deepEqual(
    heardDirect(first, [peer("claude", "m2"), peer("codex", "m3")]),
    [
      { agent: "claude", id: "m2" },
      { agent: "codex", id: "m3" },
    ],
  );
  assert.deepEqual(
    heardDirect(first, [...first, peer("new", "m4"), peer("quiet", null)]),
    [{ agent: "new", id: "m4" }],
  );
});

type Start = { freq: number; at: number };
function fakeAudio(starts: Start[]) {
  let state: AudioContextState = "suspended";
  let onState = () => {};
  const param = () => ({
    value: 0,
    setValueAtTime(value: number, at: number) {
      this.first ??= { value, at };
      return this;
    },
    first: undefined as { value: number; at: number } | undefined,
    linearRampToValueAtTime() {
      return this;
    },
    exponentialRampToValueAtTime() {
      return this;
    },
  });
  const node = () => ({
    connect<T>(target: T) {
      return target;
    },
  });
  const context = {
    get state() {
      return state;
    },
    currentTime: 0,
    destination: {},
    async resume() {
      state = "running";
    },
    suspend() {
      state = "suspended";
      onState();
    },
    addEventListener(_: string, listener: () => void) {
      onState = listener;
    },
    createGain: () => ({ ...node(), gain: param() }),
    createOscillator() {
      const frequency = param();
      return {
        ...node(),
        type: "sine",
        frequency,
        start: () => {
          const first = frequency.first;
          if (first) starts.push({ freq: first.value, at: first.at });
        },
        stop() {},
      };
    },
  };
  return context;
}
// Web Locks as a browser runs them: a request waits for the one before it
// on the same name, and one made `ifAvailable` gets null while it is held.
type Callback = (lock: { name: string } | null) => unknown;
function fakeLocks() {
  const tails = new Map<string, Promise<unknown>>();
  const held = new Set<string>();
  const grant = (name: string, callback: Callback) => {
    held.add(name);
    return Promise.resolve(callback({ name })).finally(() => held.delete(name));
  };
  return {
    request(
      name: string,
      ...args: [Callback] | [{ ifAvailable?: boolean }, Callback]
    ) {
      const [options, callback] = args.length === 1 ? [{}, args[0]] : args;
      if (options.ifAvailable && held.has(name))
        return Promise.resolve().then(() => callback(null));
      if (options.ifAvailable) held.add(name);
      const run = (tails.get(name) ?? Promise.resolve()).then(() =>
        grant(name, callback),
      );
      tails.set(name, run);
      return run;
    },
  };
}
// A wall clock every tab reads, with the timers `wait` sets.
type Clock = { now: number; timers: { at: number; fire(): void }[] };
const newClock = (): Clock => ({ now: 0, timers: [] });
function tick(clock: Clock, ms: number) {
  clock.now += ms;
  const due = clock.timers.filter((timer) => timer.at <= clock.now);
  clock.timers = clock.timers.filter((timer) => timer.at > clock.now);
  for (const timer of due) timer.fire();
}
function fakeStorage() {
  const items = new Map<string, string>();
  return {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
  };
}
const brokenStorage = {
  getItem(): string | null {
    throw new Error("storage is blocked");
  },
  setItem() {
    throw new Error("storage is blocked");
  },
};
function harness({
  locks = null as ReturnType<typeof fakeLocks> | null,
  storage = fakeStorage() as PlayerEnv["storage"],
  clock = newClock(),
} = {}) {
  const starts: Start[] = [];
  const audio = fakeAudio(starts);
  const env = {
    audio: () => audio,
    locks,
    storage,
    now: () => clock.now,
    wait: (ms: number) =>
      new Promise<void>((fire) =>
        clock.timers.push({ at: clock.now + ms, fire }),
      ),
  } as unknown as PlayerEnv;
  const player = createPlayer(env);
  return {
    player,
    audio,
    plays: () => starts.length,
    firsts: () => starts.map((start) => start.freq),
    advance(ms: number) {
      tick(clock, ms);
    },
  };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const notes = (tone: keyof typeof RECIPES) => RECIPES[tone].length;

test("sounds: nothing rings until a gesture unlocks audio, and a suspended context locks it again", async () => {
  const h = harness();
  assert.equal(h.player.kind(), "locked");
  h.player.ring("pop", ["m1"]);
  assert.equal(h.plays(), 0);
  h.player.unlock();
  await settle();
  assert.equal(h.player.kind(), "ready");
  h.player.ring("pop", ["m1"]);
  assert.deepEqual(h.firsts(), [RECIPES.pop[0]?.freq]);
  h.audio.suspend();
  assert.equal(h.player.kind(), "locked");
  h.advance(5000);
  h.player.ring("pop", ["m2"]);
  assert.equal(h.plays(), notes("pop"));
});

test("sounds: one tab rings at most once a second, and never twice for one message", async () => {
  const h = harness();
  h.player.unlock();
  await settle();
  h.player.ring("knock", ["m1"]);
  h.advance(500);
  h.player.ring("bell", ["m2"]);
  assert.equal(h.plays(), notes("knock"));
  h.advance(500);
  h.player.ring("bell", ["m2"]);
  assert.equal(h.plays(), notes("knock"), "m2 was recorded in the burst");
  h.player.ring("bell", ["m2", "m3"]);
  assert.equal(h.plays(), notes("knock") + notes("bell"));
});

test("sounds: tabs that share storage and a lock ring once for a message, whenever each hears it", async () => {
  const locks = fakeLocks();
  const storage = fakeStorage();
  const clock = newClock();
  const tabs = [0, 1, 2].map(() => harness({ locks, storage, clock }));
  for (const tab of tabs) tab.player.unlock();
  await settle();
  for (const tab of tabs) {
    tab.player.ring("chime", ["dm-1"]);
    await settle();
    tick(clock, 1500);
  }
  const plays = () => tabs.reduce((sum, tab) => sum + tab.plays(), 0);
  assert.equal(plays(), notes("chime"));
  tabs[2]?.player.ring("ping", ["dm-2"]);
  tabs[0]?.player.ring("ping", ["dm-3"]);
  await settle();
  assert.equal(plays(), notes("chime") + notes("ping"));
});

test("sounds: with storage blocked, tabs that share locks ring once for a message they hear together", async () => {
  const locks = fakeLocks();
  const clock = newClock();
  const tabs = [0, 1, 2].map(() =>
    harness({ locks, storage: brokenStorage, clock }),
  );
  for (const tab of tabs) tab.player.unlock();
  await settle();
  const plays = () => tabs.reduce((sum, tab) => sum + tab.plays(), 0);
  for (const tab of tabs) tab.player.ring("chime", ["dm-1"]);
  await settle();
  assert.equal(plays(), notes("chime"));
  tick(clock, 500);
  tabs[1]?.player.ring("ping", ["dm-2"]);
  await settle();
  assert.equal(plays(), notes("chime"), "the hold lasts a second");
  tick(clock, 1000);
  await settle();
  tabs[2]?.player.ring("ping", ["dm-3"]);
  await settle();
  assert.equal(plays(), notes("chime") + notes("ping"));
  tick(clock, 2000);
  await settle();
  tabs[2]?.player.ring("ping", ["dm-3"]);
  tabs[1]?.player.ring("ping", ["dm-2"]);
  await settle();
  assert.equal(
    plays(),
    notes("chime") + notes("ping"),
    "a tab never rings an id it heard twice",
  );
});

test("sounds: without storage or locks, a tab still rings once per message and once a second", async () => {
  for (const locks of [null, fakeLocks()]) {
    const h = harness({ locks, storage: brokenStorage });
    h.player.unlock();
    await settle();
    h.player.ring("pop", ["m1"]);
    await settle();
    h.advance(500);
    h.player.ring("pop", ["m2"]);
    await settle();
    h.advance(2000);
    h.player.ring("pop", ["m1"]);
    h.player.ring("pop", ["m2"]);
    await settle();
    assert.equal(h.plays(), notes("pop"), String(locks));
    h.player.ring("pop", ["m3"]);
    await settle();
    assert.equal(h.plays(), notes("pop") * 2, String(locks));
  }
});

test("sounds: the shared record skips known arrivals, holds new ones inside a second, and keeps the latest 200 ids", () => {
  const first = nextRing(noRings, ["a"], 10_000);
  assert.deepEqual(first, {
    play: true,
    record: { lastAt: 10_000, ids: ["a"] },
  });
  assert.deepEqual(nextRing(first.record, ["a"], 20_000), {
    play: false,
    record: first.record,
  });
  const held = nextRing(first.record, ["a", "b"], 10_999);
  assert.deepEqual(held, {
    play: false,
    record: { lastAt: 10_000, ids: ["a", "b"] },
  });
  assert.deepEqual(nextRing(held.record, ["c"], 11_000), {
    play: true,
    record: { lastAt: 11_000, ids: ["a", "b", "c"] },
  });
  assert.equal(
    nextRing(first.record, ["d"], 9_000).play,
    true,
    "a clock that moved back still rings",
  );
  let record = noRings;
  for (let n = 0; n < 250; n += 1)
    record = nextRing(record, [`m${n}`], n * 2000).record;
  assert.equal(record.ids.length, 200);
  assert.equal(record.ids[0], "m50");
  assert.equal(nextRing(record, ["m49"], 600_000).play, true);
  assert.equal(nextRing(record, ["m249"], 600_000).play, false);
});

test("sounds: a stored record that is missing or damaged reads as empty", () => {
  for (const text of [null, "", "not json", "[]", "7", "null"])
    assert.deepEqual(parseRingRecord(text), noRings, String(text));
  assert.deepEqual(
    parseRingRecord(JSON.stringify({ lastAt: 5, ids: ["a", 3, "b"] })),
    { lastAt: 5, ids: ["a", "b"] },
  );
  assert.deepEqual(
    parseRingRecord(JSON.stringify({ lastAt: "soon", ids: "a" })),
    noRings,
  );
});

test("sounds: a preview plays right away, whatever the limit", async () => {
  const h = harness();
  h.player.unlock();
  await settle();
  h.player.ring("soft", ["m1"]);
  h.player.preview("marimba");
  assert.equal(h.plays(), notes("soft") + notes("marimba"));
});
