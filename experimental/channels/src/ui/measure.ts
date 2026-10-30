import { useSyncExternalStore } from "react";

const LIMIT = 200;

export function measure(name: string, start: number): void {
  if (performance.getEntriesByName(name, "measure").length >= LIMIT)
    performance.clearMeasures(name);
  performance.measure(name, { start, end: performance.now() });
}

function useMedia(query: string): boolean {
  return useSyncExternalStore(
    (change) => {
      const list = matchMedia(query);
      list.addEventListener("change", change);
      return () => list.removeEventListener("change", change);
    },
    () => matchMedia(query).matches,
    () => false,
  );
}
// style.css turns the sidebar into a drawer at this width. The page needs it
// too, to put the one connection dot where it shows and to make the drawer
// modal only while it is one.
export function usePhone(): boolean {
  return useMedia("(max-width: 600px)");
}
// A window this short gives the member strip one row, so the timeline keeps
// its room under the banner.
export function useShortWindow(): boolean {
  return useMedia("(max-height: 560px)");
}
