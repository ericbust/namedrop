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
  type SoundChange,
  type SoundSettings,
  type Tone,
} from "../sounds.ts";

export type SoundAccess = Readonly<{
  settings: SoundSettings | null;
  player: PlayerKind;
  refresh(): void;
  change(change: SoundChange): Promise<boolean>;
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
  const refresh = useCallback(() => {
    loading.current?.abort();
    const controller = new AbortController();
    loading.current = controller;
    void poster.sounds(controller.signal).then((next) => {
      if (!controller.signal.aborted && next !== null) setSettings(next);
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
      const tone = burstTone(current.current, heard.agents);
      if (tone !== null) player.ring(tone);
    },
    [player],
  );
  useEffect(() => ring(channel), [ring, channel]);
  useEffect(() => ring(direct), [ring, direct]);
  const change = useCallback(
    async (change: SoundChange) => {
      loading.current?.abort();
      setSettings((settings) => settings && applyChange(settings, change));
      const saved = await poster.changeSound(change);
      if (saved === null) {
        refresh();
        return false;
      }
      setSettings(saved);
      return true;
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
