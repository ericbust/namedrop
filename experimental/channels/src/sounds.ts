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
export type SoundChange =
  | { kind: "default"; sound: SoundId }
  | { kind: "muted"; muted: boolean }
  | { kind: "agent"; agent: AgentKey; sound: SoundId | null };
// Agent keys whose new messages to the person arrived in one update, oldest
// first. `seq` grows by one per update that heard any.
export type Heard = Readonly<{ seq: number; agents: readonly AgentKey[] }>;
export const nothingHeard: Heard = { seq: 0, agents: [] };

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
type Locks = Pick<LockManager, "request">;
export type PlayerEnv = Readonly<{
  audio: (() => Audio) | null;
  locks: Locks | null;
  now(): number;
  wait(ms: number): Promise<void>;
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
    now: () => performance.now(),
    wait: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  };
}

export function createPlayer(env: PlayerEnv) {
  let state: PlayerState =
    env.audio === null
      ? { kind: "unavailable" }
      : { kind: "locked", context: null };
  let last = Number.NEGATIVE_INFINITY;
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
    // One sound per second, across this browser's tabs when it can tell.
    ring(tone: Tone): void {
      if (state.kind !== "ready") return;
      const audio = state.context;
      if (env.locks === null) {
        const now = env.now();
        if (now - last < GAP_MS) return;
        last = now;
        schedule(audio, tone);
        return;
      }
      void env.locks.request(LOCK, { ifAvailable: true }, async (lock) => {
        if (lock === null) return;
        schedule(audio, tone);
        await env.wait(GAP_MS);
      });
    },
  };
}
