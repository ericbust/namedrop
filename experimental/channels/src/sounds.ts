import type { AddressId } from "./model.ts";
import { ENVOY } from "./wire.ts";

export const TONES = [
  "chime",
  "ping",
  "pop",
  "knock",
  "bell",
  "blip",
  "marimba",
  "soft",
] as const;
export type Tone = (typeof TONES)[number];
export type SoundId = Tone | "none";
// The agent's pane address id, the key the server stores its sound under.
export type AgentKey = string & { readonly __brand: "AgentKey" };
export type SoundSettings = Readonly<{
  default: SoundId;
  muted: boolean;
  agents: ReadonlyMap<AgentKey, SoundId>;
}>;
// A refused save (400) means the list of agents with their own sound is full.
export type SaveResult =
  | { kind: "saved"; settings: SoundSettings }
  | { kind: "full" }
  | { kind: "failed" };
export type SoundChange =
  | { kind: "default"; sound: SoundId }
  | { kind: "muted"; muted: boolean }
  | { kind: "agent"; agent: AgentKey; sound: SoundId | null };
// A new message to the person: its author, and the id that keeps it from
// ringing twice. A direct message's id is its peer's last agent message id.
export type Arrival = Readonly<{ agent: AgentKey; id: string }>;
// The arrivals of one update, oldest first. `seq` grows by one per update
// that heard any.
export type Heard = Readonly<{ seq: number; arrivals: readonly Arrival[] }>;
export const nothingHeard: Heard = { seq: 0, arrivals: [] };

export type Note = Readonly<{
  at: number;
  freq: number;
  glide?: number;
  dur: number;
  wave: OscillatorType;
  gain: number;
  attack?: number;
}>;
export const RECIPES: Readonly<Record<Tone, readonly Note[]>> = {
  chime: [
    { at: 0, freq: 1046.5, dur: 0.5, wave: "sine", gain: 0.5 },
    { at: 0.1, freq: 1568, dur: 0.45, wave: "sine", gain: 0.4 },
  ],
  ping: [
    { at: 0, freq: 1760, dur: 0.3, wave: "sine", gain: 0.45 },
    { at: 0, freq: 3520, dur: 0.12, wave: "sine", gain: 0.08 },
  ],
  pop: [{ at: 0, freq: 640, glide: 180, dur: 0.09, wave: "sine", gain: 0.8 }],
  knock: [
    { at: 0, freq: 220, glide: 150, dur: 0.07, wave: "triangle", gain: 0.9 },
    {
      at: 0.13,
      freq: 200,
      glide: 140,
      dur: 0.07,
      wave: "triangle",
      gain: 0.75,
    },
  ],
  bell: [
    { at: 0, freq: 880, dur: 0.55, wave: "sine", gain: 0.45 },
    { at: 0, freq: 2376, dur: 0.25, wave: "sine", gain: 0.1 },
  ],
  blip: [
    { at: 0, freq: 1320, dur: 0.05, wave: "square", gain: 0.12 },
    { at: 0.07, freq: 1980, dur: 0.06, wave: "square", gain: 0.12 },
  ],
  marimba: [
    { at: 0, freq: 523.25, dur: 0.3, wave: "sine", gain: 0.6 },
    { at: 0, freq: 2093, dur: 0.05, wave: "sine", gain: 0.15 },
    { at: 0.12, freq: 784, dur: 0.3, wave: "sine", gain: 0.5 },
  ],
  soft: [
    { at: 0, freq: 392, dur: 0.5, wave: "sine", gain: 0.4, attack: 0.08 },
    {
      at: 0.12,
      freq: 587.33,
      dur: 0.42,
      wave: "sine",
      gain: 0.3,
      attack: 0.08,
    },
  ],
};
export const SOUND_NAMES: Readonly<Record<SoundId, string>> = {
  chime: "Chime",
  ping: "Ping",
  pop: "Pop",
  knock: "Knock",
  bell: "Bell",
  blip: "Blip",
  marimba: "Marimba",
  soft: "Soft",
  none: "None",
};
const VOLUME = 0.25;
const GAP_MS = 1000;
const LOCK = "namedrop:channel-sound";
const RECORD_KEY = "namedrop:channel-sound";
const KEPT_IDS = 200;

const AGENT_KEY = /^[A-Za-z0-9_-]{1,128}$/;
export function agentKey(id: AddressId | string): AgentKey | null {
  const key = ENVOY.exec(id)?.[2] ?? id;
  return AGENT_KEY.test(key) ? (key as AgentKey) : null;
}
function soundId(value: unknown): SoundId | null {
  return value === "none" || TONES.includes(value as Tone)
    ? (value as SoundId)
    : null;
}
export function parseSoundSettings(raw: unknown): SoundSettings | null {
  if (typeof raw !== "object" || raw === null) return null;
  const { default: fallback, muted, agents } = raw as Record<string, unknown>;
  const sound = soundId(fallback);
  if (sound === null || typeof muted !== "boolean") return null;
  const choices = new Map<AgentKey, SoundId>();
  if (typeof agents === "object" && agents !== null)
    for (const [id, value] of Object.entries(agents)) {
      const key = agentKey(id);
      const choice = soundId(value);
      if (key !== null && key === id && choice !== null)
        choices.set(key, choice);
    }
  return { default: sound, muted, agents: choices };
}
export function soundFor(
  settings: SoundSettings | null,
  agent: AgentKey,
): Tone | null {
  if (settings === null || settings.muted) return null;
  const sound = settings.agents.get(agent) ?? settings.default;
  return sound === "none" ? null : sound;
}
// A burst rings once, with the sound of its first agent that has one.
export function burstTone(
  settings: SoundSettings | null,
  agents: readonly AgentKey[],
): Tone | null {
  for (const agent of agents) {
    const tone = soundFor(settings, agent);
    if (tone !== null) return tone;
  }
  return null;
}
export function audible(settings: SoundSettings | null): boolean {
  return (
    settings !== null &&
    !settings.muted &&
    (settings.default !== "none" ||
      [...settings.agents.values()].some((sound) => sound !== "none"))
  );
}
export function applyChange(
  settings: SoundSettings,
  change: SoundChange,
): SoundSettings {
  switch (change.kind) {
    case "default":
      return { ...settings, default: change.sound };
    case "muted":
      return { ...settings, muted: change.muted };
    case "agent": {
      const agents = new Map(settings.agents);
      if (change.sound === null) agents.delete(change.agent);
      else agents.set(change.agent, change.sound);
      return { ...settings, agents };
    }
    default: {
      const exhaustive: never = change;
      return exhaustive;
    }
  }
}

type Audio = Pick<
  AudioContext,
  | "state"
  | "currentTime"
  | "destination"
  | "resume"
  | "createGain"
  | "createOscillator"
  | "addEventListener"
>;
// What this browser has rung, shared by its tabs: the time of the last sound
// and the ids of the latest arrivals, rung or not.
export type RingRecord = Readonly<{ lastAt: number; ids: readonly string[] }>;
export const noRings: RingRecord = {
  lastAt: Number.NEGATIVE_INFINITY,
  ids: [],
};
export function parseRingRecord(text: string | null): RingRecord {
  try {
    const raw: unknown = JSON.parse(text ?? "null");
    if (typeof raw !== "object" || raw === null) return noRings;
    const { lastAt, ids } = raw as Record<string, unknown>;
    return {
      lastAt: Number.isFinite(lastAt)
        ? (lastAt as number)
        : Number.NEGATIVE_INFINITY,
      ids: Array.isArray(ids)
        ? ids.filter((id): id is string => typeof id === "string")
        : [],
    };
  } catch {
    return noRings;
  }
}
function mergeRecords(a: RingRecord, b: RingRecord): RingRecord {
  return {
    lastAt: Math.max(a.lastAt, b.lastAt),
    ids: [...new Set([...a.ids, ...b.ids])].slice(-KEPT_IDS),
  };
}
// Arrivals already recorded never ring again. New ones ring unless a sound
// played less than a second ago; either way their ids are recorded. A clock
// that moved back does not silence sounds until it catches up.
export function nextRing(
  record: RingRecord,
  ids: readonly string[],
  now: number,
): { play: boolean; record: RingRecord } {
  if (ids.every((id) => record.ids.includes(id)))
    return { play: false, record };
  const kept = [...record.ids.filter((id) => !ids.includes(id)), ...ids].slice(
    -KEPT_IDS,
  );
  const since = now - record.lastAt;
  return since >= 0 && since < GAP_MS
    ? { play: false, record: { ...record, ids: kept } }
    : { play: true, record: { lastAt: now, ids: kept } };
}

type Locks = Pick<LockManager, "request">;
export type PlayerEnv = Readonly<{
  audio: (() => Audio) | null;
  locks: Locks | null;
  // Any call may throw, as localStorage does when site data is blocked.
  storage: Pick<Storage, "getItem" | "setItem">;
  // Wall-clock milliseconds, the clock every tab shares.
  now(): number;
}>;
// Audio stays `locked` until a gesture resumes a context; a browser can
// suspend it again later, which locks it until the next gesture.
type PlayerState =
  | { kind: "unavailable" }
  | { kind: "locked"; context: Audio | null }
  | { kind: "ready"; context: Audio };
export type PlayerKind = PlayerState["kind"];
export type Player = ReturnType<typeof createPlayer>;

function schedule(context: Audio, tone: Tone): void {
  const start = context.currentTime + 0.01;
  const out = context.createGain();
  out.gain.value = VOLUME;
  out.connect(context.destination);
  for (const note of RECIPES[tone]) {
    const at = start + note.at;
    const end = at + note.dur;
    const oscillator = context.createOscillator();
    const envelope = context.createGain();
    oscillator.type = note.wave;
    oscillator.frequency.setValueAtTime(note.freq, at);
    if (note.glide !== undefined)
      oscillator.frequency.exponentialRampToValueAtTime(note.glide, end);
    envelope.gain.setValueAtTime(0, at);
    envelope.gain.linearRampToValueAtTime(
      note.gain,
      at + (note.attack ?? 0.005),
    );
    envelope.gain.exponentialRampToValueAtTime(0.0001, end);
    oscillator.connect(envelope).connect(out);
    oscillator.start(at);
    oscillator.stop(end + 0.02);
  }
}

export function browserPlayerEnv(): PlayerEnv {
  return {
    audio: typeof AudioContext === "function" ? () => new AudioContext() : null,
    locks:
      typeof navigator.locks?.request === "function" ? navigator.locks : null,
    storage: {
      getItem: (key) => localStorage.getItem(key),
      setItem: (key, value) => localStorage.setItem(key, value),
    },
    now: () => Date.now(),
  };
}

export function createPlayer(env: PlayerEnv) {
  let state: PlayerState =
    env.audio === null
      ? { kind: "unavailable" }
      : { kind: "locked", context: null };
  // This tab's own record: it stands in when storage throws, and keeps this
  // tab from ringing one id twice whatever storage holds.
  let rung = noRings;
  const listeners = new Set<() => void>();
  const move = (next: PlayerState) => {
    state = next;
    for (const listener of listeners) listener();
  };
  function open(make: () => Audio): Audio {
    const audio = make();
    audio.addEventListener("statechange", () => {
      if (state.kind === "ready" && audio.state !== "running")
        move({ kind: "locked", context: audio });
    });
    state = { kind: "locked", context: audio };
    return audio;
  }
  function context(): Audio | null {
    if (state.kind === "unavailable" || env.audio === null) return null;
    if (state.kind === "ready") return state.context;
    const audio = state.context ?? open(env.audio);
    void audio.resume().then(
      () => {
        if (state.kind === "locked" && audio.state === "running")
          move({ kind: "ready", context: audio });
      },
      () => {},
    );
    return audio;
  }
  return {
    kind: (): PlayerKind => state.kind,
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    // Call from a user gesture: browsers start audio only then.
    unlock(): void {
      context();
    },
    preview(tone: Tone): void {
      const audio = context();
      if (audio !== null) schedule(audio, tone);
    },
    // One sound per arrival and at most one a second, across this
    // browser's tabs: the lock makes each check and record atomic.
    ring(tone: Tone, ids: readonly string[]): void {
      if (state.kind !== "ready") return;
      const audio = state.context;
      const decide = () => {
        let shared = noRings;
        try {
          shared = parseRingRecord(env.storage.getItem(RECORD_KEY));
        } catch {}
        const next = nextRing(mergeRecords(shared, rung), ids, env.now());
        rung = next.record;
        try {
          env.storage.setItem(RECORD_KEY, JSON.stringify(rung));
        } catch {}
        if (next.play) schedule(audio, tone);
      };
      if (env.locks === null) decide();
      else void env.locks.request(LOCK, decide);
    },
  };
}
