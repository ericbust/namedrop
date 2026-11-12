import assert from "node:assert/strict";
import { test } from "node:test";
import { type DirectPeer, heardDirect } from "./direct-messages.ts";
import type { AddressId, SpaceKey, State } from "./model.ts";
import {
  type AgentKey,
  agentKey,
  burstTone,
  createPlayer,
  type PlayerEnv,
  parseSoundSettings,
  RECIPES,
  type SoundSettings,
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
};
function frame(rows: Row[]) {
  return {
    key: keyA,
    frame: {
      wire: 1,
      generated_at_ms: 100,
      space: {
        key: keyA,
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
        at_ms: 100 + n,
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
function apply(state: State, rows: Row[]): State {
  const event = parseEvent("space", JSON.stringify(frame(rows)));
  assert.ok(event, "frame must parse");
  return reduce(state, event);
}
const before: Row = { id: "old", author: "claude", to: ["human"] };
const loaded = apply(initialState(), [before]);

test("sounds: the first load of a channel hears nothing, even an agent's message to the person", () => {
  assert.deepEqual(loaded.heard, { seq: 0, agents: [] });
});

test("sounds: an agent's new message to the person is heard once, by its author", () => {
  const next = apply(loaded, [
    before,
    { id: "answer", author: "claude", to: ["human", "codex"] },
  ]);
  assert.deepEqual(next.heard, { seq: 1, agents: ["claude"] });
  const again = apply(next, [
    before,
    { id: "answer", author: "claude", to: ["human", "codex"] },
  ]);
  assert.equal(again.heard, next.heard);
  const envoy = apply(next, [
    { id: "far", author: "envoy:w2:pane-address-7-8-9", to: ["you-too"] },
  ]);
  assert.deepEqual(envoy.heard, { seq: 2, agents: ["pane-address-7-8-9"] });
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

test("sounds: messages that arrive in a reconnect's replay stay silent", () => {
  let state = reduce(loaded, { type: "opened" });
  state = apply(state, [
    before,
    { id: "missed", author: "claude", to: ["human"] },
  ]);
  const ready = parseEvent("ready", JSON.stringify({ keys: [keyA] }));
  assert.ok(ready);
  state = reduce(state, ready);
  assert.equal(state.sync.kind, "live");
  assert.ok(state.channels.get(keyA as SpaceKey)?.latestMessages.has("missed"));
  assert.equal(state.heard, loaded.heard);
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

test("sounds: a direct message is heard when an agent's last message changes after the first read", () => {
  const first = [peer("claude", "m1"), peer("codex", null)];
  assert.deepEqual(heardDirect(null, first), []);
  assert.deepEqual(heardDirect(first, first), []);
  assert.deepEqual(
    heardDirect(first, [peer("claude", "m2"), peer("codex", "m3")]),
    ["claude", "codex"],
  );
  assert.deepEqual(heardDirect(first, [...first, peer("new", "m4")]), []);
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
function fakeLocks() {
  const held = new Set<string>();
  return {
    async request(
      name: string,
      _: LockOptions,
      callback: (lock: Lock | null) => Promise<void>,
    ) {
      if (held.has(name)) return callback(null);
      held.add(name);
      try {
        return await callback({ name, mode: "exclusive" });
      } finally {
        held.delete(name);
      }
    },
  };
}
function harness({ locks = null as ReturnType<typeof fakeLocks> | null } = {}) {
  const starts: Start[] = [];
  const audio = fakeAudio(starts);
  const waits: (() => void)[] = [];
  let now = 0;
  const env = {
    audio: () => audio,
    locks,
    now: () => now,
    wait: () => new Promise<void>((resolve) => waits.push(resolve)),
  } as unknown as PlayerEnv;
  const player = createPlayer(env);
  return {
    player,
    audio,
    plays: () => starts.length,
    firsts: () => starts.map((start) => start.freq),
    advance(ms: number) {
      now += ms;
      for (const release of waits.splice(0)) release();
    },
  };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const notes = (tone: keyof typeof RECIPES) => RECIPES[tone].length;

test("sounds: nothing rings until a gesture unlocks audio, and a suspended context locks it again", async () => {
  const h = harness();
  assert.equal(h.player.kind(), "locked");
  h.player.ring("pop");
  assert.equal(h.plays(), 0);
  h.player.unlock();
  await settle();
  assert.equal(h.player.kind(), "ready");
  h.player.ring("pop");
  assert.deepEqual(h.firsts(), [RECIPES.pop[0]?.freq]);
  h.audio.suspend();
  assert.equal(h.player.kind(), "locked");
  h.advance(5000);
  h.player.ring("pop");
  assert.equal(h.plays(), notes("pop"));
});

test("sounds: one tab rings at most once a second", async () => {
  const h = harness();
  h.player.unlock();
  await settle();
  h.player.ring("knock");
  h.advance(500);
  h.player.ring("bell");
  assert.equal(h.plays(), notes("knock"));
  h.advance(500);
  h.player.ring("bell");
  assert.equal(h.plays(), notes("knock") + notes("bell"));
});

test("sounds: tabs that share a lock ring once a second between them", async () => {
  const locks = fakeLocks();
  const a = harness({ locks });
  const b = harness({ locks });
  for (const tab of [a, b]) tab.player.unlock();
  await settle();
  a.player.ring("chime");
  await settle();
  b.player.ring("chime");
  a.player.ring("chime");
  await settle();
  assert.equal(a.plays() + b.plays(), notes("chime"));
  a.advance(1000);
  await settle();
  b.player.ring("ping");
  await settle();
  assert.equal(b.plays(), notes("ping"));
});

test("sounds: a preview plays right away, whatever the limit", async () => {
  const h = harness();
  h.player.unlock();
  await settle();
  h.player.ring("soft");
  h.player.preview("marimba");
  assert.equal(h.plays(), notes("soft") + notes("marimba"));
});
