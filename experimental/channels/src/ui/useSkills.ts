import { useEffect, useEffectEvent, useState } from "react";
import {
  parseSkills,
  type SkillCatalog,
  type SkillTarget,
  skillTargetKey,
} from "../skills.ts";

const TTL = 10_000;
const cache = new Map<string, { at: number; catalog: SkillCatalog }>();

export function useSkills(
  targets: readonly SkillTarget[],
): readonly SkillCatalog[] {
  const [loaded, setLoaded] = useState<{
    identity: string;
    catalogs: readonly SkillCatalog[];
  }>({ identity: "", catalogs: [] });
  const identity = targets.map(skillTargetKey).sort().join("\n");
  const snapshotTargets = useEffectEvent(() => targets);
  useEffect(() => {
    const requested = snapshotTargets();
    if (requested.length === 0) return;
    const controller = new AbortController();
    let running = false;
    async function load() {
      if (running) return;
      running = true;
      const catalogs = await Promise.all(
        requested.map(async (target) => {
          const key = skillTargetKey(target);
          const saved = cache.get(key);
          if (saved !== undefined && Date.now() - saved.at < TTL)
            return saved.catalog;
          try {
            const response = await fetch(
              `/w/${encodeURIComponent(target.key)}/participants/${encodeURIComponent(target.member)}/skills`,
              { signal: controller.signal },
            );
            if (!response.ok) return null;
            const value: unknown = await response.json();
            const parsed = parseSkills(value);
            if (parsed === null || controller.signal.aborted) return null;
            const catalog = { ...target, ...parsed };
            if (cache.size >= 100) cache.clear();
            cache.set(key, { at: Date.now(), catalog });
            return catalog;
          } catch {
            return null;
          }
        }),
      );
      running = false;
      if (!controller.signal.aborted)
        setLoaded({
          identity,
          catalogs: catalogs.filter((catalog) => catalog !== null),
        });
    }
    void load();
    const timer = setInterval(() => void load(), TTL);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [identity]);
  return loaded.identity === identity ? loaded.catalogs : [];
}
