import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  attentionConversation,
  attentionReplyTo,
  evidenceReduce,
  evidenceThread,
  type FocusedEvidence,
  messageIntent,
  prepareChannelMessage,
  replyPrefill,
} from "./attention.ts";
import type { Attention, MessageContext } from "./model.ts";
import { parseEvent, parseMessageContext } from "./wire.ts";

const raw = JSON.parse(
  readFileSync(
    new URL("./fixtures/real-frames.jsonl", import.meta.url),
    "utf8",
  ).split("\n")[0] ?? "null",
);
const event = parseEvent("space", JSON.stringify(raw));
if (event?.type !== "space") throw new Error("Missing fixture");
const channel = event.channel;
const member = [...channel.members.values()][0];
if (member === undefined) throw new Error("Missing member");
const target = { threadId: "old-thread", messageId: "old-message" };
const reply = { kind: "reply", ...target, addressId: member.id } as const;
const request = { key: channel.key, target, response: reply };
const root = {
  message_id: "root",
  thread_id: "old-thread",
  in_reply_to: null,
  at_ms: 1,
  author: "human",
  kind: "mention",
  body: { text: "Request", truncated: false },
  legs: [],
};
const anchor = {
  ...root,
  message_id: target.messageId,
  in_reply_to: "root",
  at_ms: 2,
  kind: "reply",
};
const context: MessageContext = parseMessageContext(
  {
    thread_id: target.threadId,
    message_count: 200,
    earlier_omitted: true,
    messages: [anchor, root],
  },
  target,
);

test("context validates exact thread and target, projects safe rows, and sorts messages", () => {
  assert.deepEqual(
    context.messages.map((row) => row.id),
    ["root", "old-message"],
  );
  assert.equal(context.earlierOmitted, true);
  for (const malformed of [
    {
      thread_id: "wrong",
      message_count: 2,
      earlier_omitted: false,
      messages: [root, anchor],
    },
    {
      thread_id: target.threadId,
      message_count: 1,
      earlier_omitted: false,
      messages: [root],
    },
    {
      thread_id: target.threadId,
      message_count: 2,
      earlier_omitted: false,
      messages: [root, { ...anchor, thread_id: "other" }],
    },
    {
      thread_id: target.threadId,
      message_count: 1,
      earlier_omitted: false,
      messages: [root, anchor],
    },
  ])
    assert.throws(() => parseMessageContext(malformed, target));
});
test("focused evidence ignores stale loads and failures after new navigation or retry", () => {
  const initial: FocusedEvidence = { kind: "closed", version: 0 };
  const first = evidenceReduce(initial, { type: "open", request });
  const second = evidenceReduce(first, {
    type: "open",
    request: { ...request, target: { ...target, messageId: "new" } },
  });
  assert.equal(
    evidenceReduce(second, { type: "loaded", version: first.version, context }),
    second,
  );
  assert.equal(
    evidenceReduce(second, {
      type: "failed",
      version: first.version,
      reason: "Late error",
    }),
    second,
  );
  const failed = evidenceReduce(first, {
    type: "failed",
    version: first.version,
    reason: "HTTP 404",
  });
  assert.equal(failed.kind, "error");
  const retry = evidenceReduce(failed, { type: "retry" });
  assert.equal(retry.kind, "loading");
  assert.equal(
    evidenceReduce(retry, { type: "loaded", version: first.version, context }),
    retry,
  );
  const ready = evidenceReduce(retry, {
    type: "loaded",
    version: retry.version,
    context,
  });
  assert.equal(ready.kind, "ready");
  const closed = evidenceReduce(ready, { type: "close" });
  assert.equal(
    evidenceReduce(closed, { type: "loaded", version: retry.version, context }),
    closed,
  );
});
test("historical reply target survives live frame eviction and newer replies", () => {
  const loaded = evidenceReduce(
    evidenceReduce({ kind: "closed", version: 0 }, { type: "open", request }),
    { type: "loaded", version: 1, context },
  );
  assert.equal(loaded.kind, "ready");
  if (loaded.kind !== "ready") return;
  const newer = {
    ...context.messages[1],
    id: "newer",
    threadId: target.threadId,
    inReplyTo: target.messageId,
    at: 100,
    author: member.id,
    kind: "reply",
    body: { text: "Later", truncated: false },
    legs: [],
  } as const;
  const merged = evidenceThread(loaded.context, new Map([[newer.id, newer]]));
  assert.deepEqual(
    merged.replies.map((row) => row.id),
    [target.messageId, "newer"],
  );
  assert.equal(
    loaded.request.response?.kind === "reply"
      ? loaded.request.response.messageId
      : null,
    target.messageId,
  );
  assert.equal(loaded.request.response?.addressId, member.id);
  assert.equal(
    evidenceThread(loaded.context, new Map()).replies[0]?.id,
    target.messageId,
  );
  const sent = evidenceReduce(loaded, {
    type: "sent",
    key: channel.key,
    messageId: target.messageId,
  });
  assert.ok(sent.kind === "ready");
  assert.equal(sent.request.response, null);
  assert.equal(sent.context, loaded.context);
});
test("reply intent addresses the responsible member, preserves typed drafts and rejects retired or missing identities", () => {
  const directory = { members: channel.members, homes: new Map() };
  const to = attentionReplyTo(reply, directory);
  assert.deepEqual(to, { names: [member.alias], mentions: member.alias });
  assert.ok(to);
  assert.deepEqual(replyPrefill("", to), {
    text: `${member.alias} `,
    preserved: false,
  });
  assert.deepEqual(replyPrefill("@someone keep my draft", to), {
    text: "@someone keep my draft",
    preserved: true,
  });
  assert.equal(
    attentionReplyTo(reply, { ...directory, members: new Map() }),
    null,
  );
  assert.equal(
    attentionReplyTo(reply, {
      ...directory,
      members: new Map([[member.id, { ...member, activity: "retired" }]]),
    }),
    null,
  );
});
test("legacy attention links only to an available loaded message and does not invent a response", () => {
  const attention: Attention = {
    rowKey: "legacy",
    source: "delivery_attempt",
    messageId: target.messageId,
    subjectAlias: member.alias,
    summary: { text: "Old", truncated: false },
    subjectSummary: null,
    openedAt: 1,
  };
  assert.equal(attentionConversation(attention, channel), null);
  assert.deepEqual(
    attentionConversation(attention, {
      messages: new Map(context.messages.map((row) => [row.id, row])),
    }),
    target,
  );
});

test("tracked response prepares visible channel text with the owner and exact item reference", () => {
  const item: Attention = {
    rowKey: "decision",
    source: "coordination",
    coordinationId: "coordination-1",
    response: { kind: "message", addressId: member.id },
    conversation: target,
    summary: { text: "Decision", truncated: false },
    subjectSummary: null,
    openedAt: 1,
    subjectAlias: null,
    messageId: null,
  };
  const intent = messageIntent(item, target, true);
  assert.ok(intent);
  const directory = { members: channel.members, homes: new Map() };
  assert.deepEqual(prepareChannelMessage(intent, directory, "", false), {
    kind: "prepared",
    text: `${member.alias} Regarding coordination coordination-1: `,
  });
  assert.deepEqual(
    prepareChannelMessage(intent, directory, "My authored draft", false),
    { kind: "preserved" },
  );
  assert.deepEqual(prepareChannelMessage(intent, directory, "", true), {
    kind: "preserved",
  });
  assert.deepEqual(
    prepareChannelMessage(
      intent,
      { ...directory, members: new Map() },
      "",
      false,
    ),
    { kind: "unavailable" },
  );
  assert.deepEqual(
    prepareChannelMessage(
      intent,
      {
        ...directory,
        members: new Map([
          [member.id, { ...member, kind: { type: "terminal" } }],
        ]),
      },
      "",
      false,
    ),
    { kind: "unavailable" },
  );
  const message = messageIntent(
    { ...item, source: "delivery_attempt", coordinationId: null },
    target,
    false,
  );
  assert.ok(message);
  assert.equal(message.preparation, "idle");
  assert.deepEqual(prepareChannelMessage(message, directory, "", false), {
    kind: "prepared",
    text: `${member.alias} Regarding message old-message: `,
  });
});
test("channel preparation retries after preserved drafts without losing the tracked evidence boundary", () => {
  const response = {
    kind: "message",
    addressId: member.id,
    reference: { kind: "coordination", id: "coordination-1" },
    preparation: "requested",
  } as const;
  let state = evidenceReduce(
    { kind: "closed", version: 0 },
    { type: "open", request: { ...request, response } },
  );
  state = evidenceReduce(state, {
    type: "loaded",
    version: state.version,
    context,
  });
  state = evidenceReduce(state, {
    type: "message-prepared",
    version: state.version,
    result: "preserved",
  });
  assert.ok(
    state.kind === "ready" && state.request.response?.kind === "message",
  );
  assert.equal(state.request.response.preparation, "preserved");
  const retried = evidenceReduce(state, {
    type: "prepare-message",
    draftEmpty: false,
  });
  assert.ok(
    retried.kind === "ready" && retried.request.response?.kind === "message",
  );
  assert.equal(retried.request.response.preparation, "requested");
  const prepared = evidenceReduce(retried, {
    type: "message-prepared",
    version: retried.version,
    result: "prepared",
  });
  const sent = evidenceReduce(prepared, {
    type: "sent",
    key: channel.key,
    messageId: null,
  });
  assert.ok(sent.kind === "ready" && sent.request.response?.kind === "message");
  assert.equal(sent.request.response.preparation, "idle");
  assert.equal(sent.context, context);
  assert.equal(
    evidenceReduce(sent, {
      type: "message-prepared",
      version: sent.version - 1,
      result: "prepared",
    }),
    sent,
  );
});

test("channel preparation is idempotent while requested or already prepared", () => {
  for (const preparation of ["requested", "prepared"] as const) {
    const response = {
      kind: "message",
      addressId: member.id,
      reference: { kind: "coordination", id: "coordination-1" },
      preparation,
    } as const;
    const state = evidenceReduce(
      evidenceReduce(
        { kind: "closed", version: 0 },
        {
          type: "open",
          request: { ...request, response },
        },
      ),
      { type: "loaded", version: 1, context },
    );
    assert.equal(
      evidenceReduce(state, { type: "prepare-message", draftEmpty: false }),
      state,
    );
    const cleared = evidenceReduce(state, {
      type: "prepare-message",
      draftEmpty: true,
    });
    if (preparation === "requested") assert.equal(cleared, state);
    else {
      assert.ok(
        cleared.kind === "ready" &&
          cleared.request.response?.kind === "message",
      );
      assert.equal(cleared.request.response.preparation, "requested");
    }
  }
});
