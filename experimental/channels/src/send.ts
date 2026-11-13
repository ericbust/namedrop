import type { ImageUpload } from "./images.ts";
import type {
  AddressId,
  Alias,
  ControlTarget,
  DeliverAnswer,
  DeliverFailure,
  DeliverResult,
  SafeText,
  SendFailure,
  SpaceKey,
} from "./model.ts";
import { type ReminderConversation, reminderPath } from "./reminders.ts";
import {
  parseSoundSettings,
  type SaveResult,
  type SoundSave,
  type SoundSettings,
} from "./sounds.ts";
import {
  parseDeliverAnswer,
  parseFullBody,
  parseImageUpload,
  parseReceipt,
  parseRefusal,
  parseReminders,
  parseSessionToken,
} from "./wire.ts";

export type Post = {
  requestId: string;
  key: SpaceKey;
  source: string;
  inReplyTo: string | null;
  directTo?: AddressId;
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
    async reminders(target: ReminderConversation, signal: AbortSignal) {
      const response = await fetcher(reminderPath(target), {
        credentials: "same-origin",
        signal,
      });
      if (!response.ok) throw new Error("Could not load reminders");
      return parseReminders(await response.json());
    },
    async cancelReminder(
      target: ReminderConversation,
      id: string,
      signal: AbortSignal,
    ): Promise<ControlResult> {
      try {
        const response = await postWithTokenRetry(
          `${reminderPath(target)}/${encodeURIComponent(id)}/cancel`,
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
    prepare(): void {
      void token();
    },
    async sounds(signal: AbortSignal): Promise<SoundSettings | null> {
      try {
        const response = await fetcher("/api/sounds", {
          credentials: "same-origin",
          signal,
        });
        return response.ok ? parseSoundSettings(await response.json()) : null;
      } catch {
        return null;
      }
    },
    async changeSound(save: SoundSave): Promise<SaveResult> {
      try {
        const response = await postWithTokenRetry("/api/sounds", save);
        if (response.status === 400) return { kind: "full" };
        const settings =
          response.ok || response.status === 409
            ? parseSoundSettings(await response.json())
            : null;
        if (settings === null) return { kind: "failed" };
        return response.ok
          ? { kind: "saved", settings }
          : { kind: "conflict", settings };
      } catch {
        return { kind: "failed" };
      }
    },
    async fullBody(
      key: SpaceKey,
      messageId: string,
      directTo?: AddressId,
    ): Promise<SafeText | null> {
      const conversation =
        directTo === undefined
          ? `/w/${key}`
          : `/w/${key}/direct-messages/${encodeURIComponent(directTo)}`;
      const path = `${conversation}/messages/${encodeURIComponent(messageId)}/body`;
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
    // Stores one image for a message to name. The server reads the type from
    // the bytes, so the body goes up as it is.
    async uploadImage(file: Blob): Promise<ImageUpload> {
      const send = (csrf: string | null) =>
        fetcher("/api/images", {
          method: "POST",
          credentials: "same-origin",
          headers: {
            "Content-Type": "application/octet-stream",
            ...(csrf === null ? {} : { "X-Namedrop-Csrf": csrf }),
          },
          body: file,
        });
      try {
        const used = token();
        let response = await send(await used);
        if (response.status === 403) {
          if (session === used) session = null;
          response = await send(await token());
        }
        if (response.ok)
          return {
            kind: "stored",
            image: parseImageUpload(await response.json()),
          };
        return {
          kind: "refused",
          why:
            response.status === 415
              ? "type"
              : response.status === 413
                ? "size"
                : "failed",
        };
      } catch {
        return { kind: "refused", why: "failed" };
      }
    },
    async post(post: Post): Promise<PostResult> {
      try {
        const path =
          post.directTo === undefined
            ? `/w/${post.key}/messages`
            : `/w/${post.key}/direct-messages/${encodeURIComponent(post.directTo)}/messages`;
        const response = await postWithTokenRetry(path, {
          request_id: post.requestId,
          source: post.source,
          ...(post.directTo !== undefined || post.inReplyTo === null
            ? {}
            : { in_reply_to: post.inReplyTo }),
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
      target: ControlTarget,
      signal: AbortSignal,
    ): Promise<ControlResult> {
      try {
        // A dismiss is posted only after the person confirmed it.
        const response = await postWithTokenRetry(
          target.kind === "thread"
            ? `/w/${key}/threads/${encodeURIComponent(target.threadId)}/${target.action}`
            : `/w/${key}/attempts/${encodeURIComponent(target.attemptId)}/dismiss`,
          target.kind === "thread" ? {} : { confirmed: true },
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
