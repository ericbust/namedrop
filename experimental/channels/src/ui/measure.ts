import { useSyncExternalStore } from "react";

const LIMIT = 200;

export function measure(name: string, start: number): void {
  if (performance.getEntriesByName(name, "measure").length >= LIMIT)
    performance.clearMeasures(name);
  performance.measure(name, { start, end: performance.now() });
}

// One subscribe per query, made once, so React keeps the listener across
// renders instead of swapping it on every keystroke.
function mediaHook(query: string): () => boolean {
  const subscribe = (change: () => void) => {
    const list = matchMedia(query);
    list.addEventListener("change", change);
    return () => list.removeEventListener("change", change);
  };
  const snapshot = () => matchMedia(query).matches;
  const serverSnapshot = () => false;
  return () => useSyncExternalStore(subscribe, snapshot, serverSnapshot);
}
// style.css turns the sidebar into a drawer at this width. The page needs it
// too, to put the one connection dot where it shows and to make the drawer
// modal only while it is one.
export const usePhone = mediaHook("(max-width: 600px)");
// A window this short gives the member strip one row, so the timeline keeps
// its room under the banner.
export const useShortWindow = mediaHook("(max-height: 560px)");
