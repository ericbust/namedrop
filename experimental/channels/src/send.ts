import type { Draft, OutgoingPhase } from "./model.ts";
import { parseReceipt, parseRefusal, parseSessionToken } from "./wire.ts";

export type Post = Draft & { requestId: string };
export type PostResult =
  | { kind: "sent"; messageId: string }
  | Extract<OutgoingPhase, { kind: "failed" }>;

export function createPoster(fetcher: typeof fetch = fetch) {
  let session: Promise<string | null> | null = null;
  const token = () => {
    session ??= fetcher("/api/session", { credentials: "same-origin" })
      .then(async (response) =>
        response.ok ? parseSessionToken(await response.json()) : null,
      )
      .catch(() => null);
    return session;
  };
  async function attempt(post: Post, csrf: string | null): Promise<Response> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (csrf !== null) headers["X-Namedrop-Csrf"] = csrf;
    return fetcher(`/w/${post.key}/messages`, {
      method: "POST",
      credentials: "same-origin",
      headers,
      body: JSON.stringify({
        request_id: post.requestId,
        source: post.source,
        ...(post.inReplyTo === null ? {} : { in_reply_to: post.inReplyTo }),
      }),
    });
  }
  return {
    prepare(): void {
      void token();
    },
    async post(post: Post): Promise<PostResult> {
      try {
        const used = token();
        let response = await attempt(post, await used);
        if (response.status === 403) {
          if (session === used) session = null;
          response = await attempt(post, await token());
        }
        if (!response.ok)
          return {
            kind: "failed",
            status: response.status,
            message: parseRefusal(await response.text()),
          };
        try {
          return {
            kind: "sent",
            messageId: parseReceipt(await response.json()),
          };
        } catch {
          return { kind: "failed", status: response.status, message: "" };
        }
      } catch {
        return { kind: "failed", status: 0, message: "" };
      }
    },
  };
}
