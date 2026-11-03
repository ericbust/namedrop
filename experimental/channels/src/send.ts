import type {
  AddressId,
  Alias,
  DeliverAnswer,
  DeliverFailure,
  DeliverResult,
  SafeText,
  SendFailure,
  SpaceKey,
  ThreadAction,
} from "./model.ts";
import {
  parseDeliverAnswer,
  parseFullBody,
  parseReceipt,
  parseRefusal,
  parseSessionToken,
} from "./wire.ts";

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
export type ReviewPost = {
  requestId: string;
  reviewer: AddressId;
  coordinationIds: readonly string[];
};
export type ReviewResult =
  | { kind: "sent"; messageId: string }
  | { kind: "failed"; status: number; message: string };

// A 2xx status is the server's answer; a body that stalls or breaks after it
// must not hold the press. Reading has locked the body, so only aborting the
// request releases its connection.
function bodyWithin(
  response: Response,
  ms: number,
  request: AbortController,
): Promise<string> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<string>((resolve) => {
    timer = setTimeout(() => {
      request.abort();
      resolve("");
    }, ms);
  });
  return Promise.race([response.text().catch(() => ""), late]).finally(() =>
    clearTimeout(timer),
  );
}

export function createPoster(fetcher: typeof fetch = fetch, bodyWait = 5_000) {
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
    signal: AbortSignal | null,
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
      signal,
    });
  }
  async function postWithTokenRetry(
    path: string,
    body: object,
    signal: AbortSignal | null = null,
  ): Promise<Response> {
    const used = token();
    const response = await attempt(path, body, await used, signal);
    if (response.status !== 403) return response;
    if (session === used) session = null;
    return attempt(path, body, await token(), signal);
  }
  function read(path: string, csrf: string | null): Promise<Response> {
    return fetcher(path, {
      credentials: "same-origin",
      headers: csrf === null ? {} : { "X-Namedrop-Csrf": csrf },
    });
  }
  return {
    prepare(): void {
      void token();
    },
    async fullBody(key: SpaceKey, messageId: string): Promise<SafeText | null> {
      const path = `/w/${key}/messages/${encodeURIComponent(messageId)}/body`;
      try {
        const used = token();
        let response = await read(path, await used);
        if (response.status === 403) {
          if (session === used) session = null;
          response = await read(path, await token());
        }
        return response.ok
          ? parseFullBody(await response.json(), messageId)
          : null;
      } catch {
        return null;
      }
    },
    async post(post: Post): Promise<PostResult> {
      try {
        const response = await postWithTokenRetry(`/w/${post.key}/messages`, {
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
      signal: AbortSignal,
    ): Promise<ControlResult> {
      try {
        const response = await postWithTokenRetry(
          `/w/${key}/threads/${encodeURIComponent(threadId)}/${action}`,
          {},
          signal,
        );
        return response.ok
          ? { kind: "done" }
          : { kind: "failed", status: response.status };
      } catch {
        return { kind: "failed", status: 0 };
      }
    },
    async review(key: SpaceKey, post: ReviewPost): Promise<ReviewResult> {
      try {
        const response = await postWithTokenRetry(
          `/w/${key}/coordinations/review`,
          {
            request_id: post.requestId,
            reviewer: post.reviewer,
            coordination_ids: post.coordinationIds,
          },
        );
        const body = await response.text();
        if (!response.ok)
          return {
            kind: "failed",
            status: response.status,
            message: parseRefusal(body),
          };
        return { kind: "sent", messageId: parseReceipt(JSON.parse(body)) };
      } catch {
        return { kind: "failed", status: 0, message: "" };
      }
    },
    async deliver(
      key: SpaceKey,
      messageId: string,
      to: readonly Alias[],
    ): Promise<DeliverAnswer> {
      const each = (failure: DeliverFailure): DeliverAnswer => ({
        results: new Map(
          to.map((alias): [Alias, DeliverResult] => [
            alias,
            { kind: "failed", ...failure },
          ]),
        ),
        channel: null,
      });
      try {
        const request = new AbortController();
        const response = await postWithTokenRetry(
          `/w/${key}/messages/${encodeURIComponent(messageId)}/deliver`,
          { to: to.map((alias) => alias.slice(1)) },
          request.signal,
        );
        const body = await bodyWithin(response, bodyWait, request);
        return response.ok
          ? parseDeliverAnswer(body, to)
          : each({
              cause: "refused",
              status: response.status,
              message: parseRefusal(body),
            });
      } catch {
        return each({ cause: "refused", status: 0, message: "" });
      }
    },
  };
}
