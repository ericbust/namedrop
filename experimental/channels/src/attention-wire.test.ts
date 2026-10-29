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
    assert.equal(item?.response?.kind, paneOnly ? "pane" : "reply");
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
