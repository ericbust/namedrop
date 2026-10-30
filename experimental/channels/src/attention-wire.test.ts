import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseEvent } from "./wire.ts";

const fixture = JSON.parse(
  readFileSync(
    new URL("./fixtures/real-frames.jsonl", import.meta.url),
    "utf8",
  ).split("\n")[0] ?? "null",
);
const sources = [
  "delivery_attempt",
  "agent_disposition",
  "automatic_reply_limit",
  "coordination",
  "agent_status",
  "context_grant",
  "pane_binding",
  "integration_registration",
];
for (const source of sources) {
  test(`attention wire preserves ${source} evidence and safe response independently of loaded messages`, () => {
    const value = structuredClone(fixture);
    const paneOnly = [
      "agent_status",
      "context_grant",
      "pane_binding",
      "integration_registration",
    ].includes(source);
    value.frame.attention = [
      {
        attention_id: `attention-${source}`,
        source_id: `source-${source}`,
        source_kind: source,
        coordination_id: source === "coordination" ? "coordination-1" : null,
        summary: { text: "Needs you", truncated: false },
        opened_at_ms: 10,
        conversation: { thread_id: "old-thread", message_id: "old-message" },
        response: paneOnly
          ? { kind: "pane", address_id: "responsible" }
          : source === "coordination"
            ? { kind: "message", address_id: "responsible" }
            : {
                kind: "reply",
                thread_id: "old-thread",
                message_id: "old-message",
                address_id: "responsible",
              },
      },
    ];
    const event = parseEvent("space", JSON.stringify(value));
    assert.ok(event?.type === "space");
    const item = event.channel.attention[0];
    assert.deepEqual(item?.conversation, {
      threadId: "old-thread",
      messageId: "old-message",
    });
    assert.equal(item?.rowKey, `attention-${source}`);
    assert.equal(item?.sourceId, `source-${source}`);
    assert.equal(
      item?.response?.kind,
      paneOnly ? "pane" : source === "coordination" ? "message" : "reply",
    );
    assert.equal(
      item?.coordinationId,
      source === "coordination" ? "coordination-1" : null,
    );
    assert.equal(event.channel.messages.size, 0);
    value.frame.attention[0].summary.text = "Updated explanation";
    const updated = parseEvent("space", JSON.stringify(value));
    assert.ok(updated?.type === "space");
    assert.equal(updated.channel.attention[0]?.rowKey, item?.rowKey);
  });
}
test("attention wire rejects a reply anchor that differs from its evidence", () => {
  const value = structuredClone(fixture);
  value.frame.attention = [
    {
      source_kind: "coordination",
      summary: { text: "Decision", truncated: false },
      opened_at_ms: 1,
      conversation: { thread_id: "thread", message_id: "evidence" },
      response: {
        kind: "reply",
        thread_id: "thread",
        message_id: "different",
        address_id: "owner",
      },
    },
  ];
  assert.equal(parseEvent("space", JSON.stringify(value)), null);
});
test("local integration response carries no invented target", () => {
  const value = structuredClone(fixture);
  value.frame.attention = [
    {
      source_kind: "integration_registration",
      summary: { text: "Repair", truncated: false },
      opened_at_ms: 1,
      conversation: null,
      response: { kind: "local" },
    },
  ];
  const event = parseEvent("space", JSON.stringify(value));
  assert.ok(event?.type === "space");
  assert.deepEqual(event.channel.attention[0]?.response, { kind: "local" });
  assert.equal(event.channel.attention[0]?.conversation, null);
});

test("message response validates its stable owner and requires scoped evidence", () => {
  const value = structuredClone(fixture);
  value.frame.attention = [
    {
      source_kind: "delivery_attempt",
      summary: { text: "Tracked request", truncated: false },
      opened_at_ms: 1,
      conversation: { thread_id: "thread", message_id: "request" },
      response: { kind: "message", address_id: "owner" },
    },
  ];
  const event = parseEvent("space", JSON.stringify(value));
  assert.ok(event?.type === "space");
  assert.deepEqual(event.channel.attention[0]?.response, {
    kind: "message",
    addressId: "owner",
  });
  for (const address_id of [null, "", 42]) {
    const malformed = structuredClone(value);
    malformed.frame.attention[0].response.address_id = address_id;
    assert.equal(parseEvent("space", JSON.stringify(malformed)), null);
  }
  value.frame.attention[0].conversation = null;
  assert.equal(parseEvent("space", JSON.stringify(value)), null);
});

test("attention wire reads the thread a reply-limit item names and the actions it allows, and a malformed list never drops the item", () => {
  const parse = (extra: Record<string, unknown>) => {
    const value = structuredClone(fixture);
    value.frame.attention = [
      {
        source_kind: "automatic_reply_limit",
        summary: { text: "Paused", truncated: false },
        opened_at_ms: 10,
        ...extra,
      },
    ];
    const event = parseEvent("space", JSON.stringify(value));
    assert.ok(event?.type === "space");
    const [item] = event.channel.attention;
    assert.ok(item !== undefined, JSON.stringify(extra));
    return [item.threadId, item.actions];
  };
  assert.deepEqual(parse({ thread_id: "t1", actions: ["continue", "stop"] }), [
    "t1",
    ["continue", "stop"],
  ]);
  assert.deepEqual(
    parse({
      thread_id: "t1",
      actions: ["stop", "rewind", 7, "stop", null, "continue"],
    }),
    ["t1", ["stop", "continue"]],
    "unknown words and repeats drop",
  );
  assert.deepEqual(parse({}), [null, []]);
  assert.deepEqual(parse({ thread_id: null, actions: null }), [null, []]);
  assert.deepEqual(parse({ thread_id: 42, actions: "continue" }), [null, []]);
  assert.deepEqual(parse({ thread_id: "  ", actions: {} }), [null, []]);
});
