import { peekInputCopy } from "./copy.ts";
import type { PeekTarget } from "./model.ts";
import { parseSessionToken } from "./wire.ts";

export type PeekInput =
  | { kind: "text"; text: string; continuity: string }
  | {
      kind: "key";
      key: "Enter" | "Escape" | "Ctrl-C" | "ArrowUp" | "ArrowDown";
      continuity: string;
    };

let session: Promise<string> | null = null;
function token(): Promise<string> {
  session ??= fetch("/api/session", { credentials: "same-origin" })
    .then(async (response) => {
      if (!response.ok) throw new Error("Session unavailable");
      return parseSessionToken(await response.json());
    })
    .catch((error: unknown) => {
      session = null;
      throw error;
    });
  return session;
}

export async function sendPeekInput(
  target: PeekTarget,
  input: PeekInput,
  signal: AbortSignal,
): Promise<void> {
  const path = `/w/${target.channel}/participants/${encodeURIComponent(target.member)}/input`;
  const attempt = async (csrf: string) =>
    fetch(path, {
      method: "POST",
      credentials: "same-origin",
      headers: {
        "Content-Type": "application/json",
        "X-Namedrop-Csrf": csrf,
      },
      body: JSON.stringify(input),
      signal,
    });
  const used = token();
  let response = await attempt(await used);
  if (response.status === 403) {
    if (session === used) session = null;
    response = await attempt(await token());
  }
  if (response.status === 409)
    throw new Error(peekInputCopy.busy(target.alias));
  if (!response.ok) throw new Error(peekInputCopy.refused(response.status));
}
