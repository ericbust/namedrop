import type { SendFailure, SpaceKey, ThreadAction } from "./model.ts";
import { parseReceipt, parseRefusal, parseSessionToken } from "./wire.ts";

export type Post = {
  requestId: string;
  key: SpaceKey;
  source: string;
  inReplyTo: string | null;
};
export type PostResult =
  | { kind: "sent"; messageId: string }
  | ({ kind: "failed" } & SendFailure);
export type ControlResult =
  | { kind: "done" }
  | { kind: "failed"; status: number };

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
  function attempt(
    path: string,
    body: object,
    csrf: string | null,
  ): Promise<Response> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (csrf !== null) headers["X-Namedrop-Csrf"] = csrf;
    return fetcher(path, {
      method: "POST",
      credentials: "same-origin",
      headers,
      body: JSON.stringify(body),
    });
  }
  // A 403 means the token went stale, so it is read again once.
  async function send(path: string, body: object): Promise<Response> {
    const used = token();
    const response = await attempt(path, body, await used);
    if (response.status !== 403) return response;
    if (session === used) session = null;
    return attempt(path, body, await token());
  }
  return {
    prepare(): void {
      void token();
    },
    async post(post: Post): Promise<PostResult> {
      try {
        const response = await send(`/w/${post.key}/messages`, {
          request_id: post.requestId,
          source: post.source,
          ...(post.inReplyTo === null ? {} : { in_reply_to: post.inReplyTo }),
        });
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
    async control(
      key: SpaceKey,
      threadId: string,
      action: ThreadAction,
    ): Promise<ControlResult> {
      try {
        const response = await send(
          `/w/${key}/threads/${encodeURIComponent(threadId)}/${action}`,
          {},
        );
        return response.ok
          ? { kind: "done" }
          : { kind: "failed", status: response.status };
      } catch {
        return { kind: "failed", status: 0 };
      }
    },
  };
}
