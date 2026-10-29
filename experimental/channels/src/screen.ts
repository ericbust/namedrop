import type { PeekTarget, ScreenResult } from "./model.ts";
import { parseScreen, screenFailure } from "./wire.ts";

export type ScreenReader = (
  target: PeekTarget,
  signal: AbortSignal,
) => Promise<ScreenResult>;
export function createScreenReader(
  fetcher: typeof fetch = fetch,
): ScreenReader {
  return async (target, signal) => {
    try {
      const response = await fetcher(
        `/w/${target.channel}/participants/${encodeURIComponent(target.member)}/screen`,
        { credentials: "same-origin", cache: "no-store", signal },
      );
      return response.ok
        ? { kind: "text", screen: parseScreen(await response.text()) }
        : { kind: "failed", failure: screenFailure(response.status) };
    } catch {
      return { kind: "failed", failure: "unreachable" };
    }
  };
}
