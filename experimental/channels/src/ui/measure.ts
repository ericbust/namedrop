import { useSyncExternalStore } from "react";

const LIMIT = 200;

export function measure(name: string, start: number): void {
  if (performance.getEntriesByName(name, "measure").length >= LIMIT)
    performance.clearMeasures(name);
  performance.measure(name, { start, end: performance.now() });
}

// style.css turns the sidebar into a drawer at this width. The page needs it
// too, to put the one connection dot where it shows and to make the drawer
// modal only while it is one.
const PHONE = "(max-width: 600px)";

function onPhoneChange(change: () => void): () => void {
  const query = matchMedia(PHONE);
  query.addEventListener("change", change);
  return () => query.removeEventListener("change", change);
}
export function usePhone(): boolean {
  return useSyncExternalStore(onPhoneChange, () => matchMedia(PHONE).matches);
}
