const LIMIT = 200;

export function measure(name: string, start: number): void {
  if (performance.getEntriesByName(name, "measure").length >= LIMIT)
    performance.clearMeasures(name);
  performance.measure(name, { start, end: performance.now() });
}
