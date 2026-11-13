import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { createPoster } from "../send.ts";
import {
  applyChange,
  browserPlayerEnv,
  burstTone,
  createPlayer,
  type Heard,
  type PlayerKind,
  type SaveResult,
  type SoundChange,
  type SoundSettings,
  saveOf,
  type Tone,
} from "../sounds.ts";

export type SoundAccess = Readonly<{
  settings: SoundSettings | null;
  player: PlayerKind;
  refresh(): void;
  change(change: SoundChange): Promise<SaveResult["kind"]>;
  preview(tone: Tone): void;
  unlock(): void;
}>;
export const SoundContext = createContext<SoundAccess | null>(null);
export function useSoundAccess(): SoundAccess | null {
  return useContext(SoundContext);
}

const GESTURES = ["pointerdown", "pointerup", "keydown"] as const;

export function useSounds(
  poster: ReturnType<typeof createPoster>,
  channel: Heard,
  direct: Heard,
): SoundAccess {
  const [player] = useState(() => createPlayer(browserPlayerEnv()));
  const kind = useSyncExternalStore(player.subscribe, player.kind);
  const [settings, setSettings] = useState<SoundSettings | null>(null);
  const current = useRef(settings);
  current.current = settings;
  const loading = useRef<AbortController | null>(null);
  // Every load and save takes the next number; only the latest request's
  // answer is shown, so a late answer never brings back an older choice.
  const latest = useRef(0);
  // Saves go one at a time, so the server gets this tab's picks in order.
  const saving = useRef<Promise<unknown>>(Promise.resolve());
  const refresh = useCallback(() => {
    const request = ++latest.current;
    loading.current?.abort();
    const controller = new AbortController();
    loading.current = controller;
    // A load waits behind this tab's queued saves, so it reads what they
    // stored instead of the value from before them.
    void saving.current
      .then(() => poster.sounds(controller.signal))
      .then((next) => {
        if (request === latest.current && next !== null) setSettings(next);
      });
  }, [poster]);
  useEffect(() => {
    refresh();
    const shown = () => {
      if (!document.hidden) refresh();
    };
    document.addEventListener("visibilitychange", shown);
    return () => {
      document.removeEventListener("visibilitychange", shown);
      loading.current?.abort();
    };
  }, [refresh]);
  useEffect(() => {
    if (kind !== "locked") return;
    const unlock = () => player.unlock();
    for (const name of GESTURES)
      document.addEventListener(name, unlock, { capture: true, passive: true });
    return () => {
      for (const name of GESTURES)
        document.removeEventListener(name, unlock, true);
    };
  }, [kind, player]);
  const ring = useCallback(
    (heard: Heard) => {
      const tone = burstTone(
        current.current,
        heard.arrivals.map((arrival) => arrival.agent),
      );
      if (tone !== null)
        player.ring(
          tone,
          heard.arrivals.map((arrival) => arrival.id),
        );
    },
    [player],
  );
  useEffect(() => ring(channel), [ring, channel]);
  useEffect(() => ring(direct), [ring, direct]);
  const change = useCallback(
    async (change: SoundChange): Promise<SaveResult["kind"]> => {
      const shown = current.current;
      if (shown === null) return "failed";
      const request = ++latest.current;
      loading.current?.abort();
      const save = saveOf(shown, change);
      current.current = applyChange(shown, change);
      setSettings(current.current);
      const run = saving.current.then(() => poster.changeSound(save));
      saving.current = run;
      const saved = await run;
      if (request !== latest.current) return saved.kind;
      if (saved.kind === "saved" || saved.kind === "conflict")
        setSettings(saved.settings);
      else refresh();
      return saved.kind;
    },
    [poster, refresh],
  );
  return useMemo(
    () => ({
      settings,
      player: kind,
      refresh,
      change,
      preview: player.preview,
      unlock: player.unlock,
    }),
    [settings, kind, refresh, change, player],
  );
}
