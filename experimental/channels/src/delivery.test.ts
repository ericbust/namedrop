import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type Deliveries,
  type Delivery,
  delivery,
  deliveryOf,
  noDeliveries,
  settleDeliveries,
  stepDelivery,
} from "./delivery.ts";
import type { AddressId, Member, Message, SpaceKey } from "./model.ts";

const idle: Delivery = { kind: "idle" };
const sending: Delivery = { kind: "sending" };
const sent: Delivery = { kind: "sent" };
const refused: Delivery = {
  kind: "failed",
  status: 400,
  message: "@codex is not an agent here.",
};

test("delivery: a press sends once, the answer says sent or failed, and Try again sends again", () => {
  assert.deepEqual(delivery(idle, { kind: "start" }), sending);
  assert.deepEqual(delivery(refused, { kind: "start" }), sending);
  assert.equal(delivery(sending, { kind: "start" }), sending, "one POST");
  assert.equal(delivery(sent, { kind: "start" }), sent, "sent stays sent");
  assert.deepEqual(delivery(sending, { kind: "done" }), sent);
  assert.deepEqual(
    delivery(sending, {
      kind: "failed",
      status: 400,
      message: "@codex is not an agent here.",
    }),
    refused,
  );
  for (const state of [idle, sent, refused]) {
    assert.equal(delivery(state, { kind: "done" }), state);
    assert.equal(
      delivery(state, { kind: "failed", status: 0, message: "" }),
      state,
    );
  }
});

const key = "a".repeat(20) as SpaceKey;
const codex = { key, messageId: "p1", alias: "@codex" } as const;
const claude = { key, messageId: "p1", alias: "@claude" } as const;

test("deliveries: each offer keeps its own state, and an unchanged step keeps the same map", () => {
  assert.deepEqual(deliveryOf(noDeliveries, codex), idle);
  const started = stepDelivery(noDeliveries, codex, { kind: "start" });
  assert.deepEqual(deliveryOf(started, codex), sending);
  assert.deepEqual(deliveryOf(started, claude), idle);
  assert.equal(stepDelivery(started, codex, { kind: "start" }), started);
  assert.equal(stepDelivery(started, claude, { kind: "done" }), started);
  const both = stepDelivery(
    stepDelivery(started, codex, { kind: "done" }),
    claude,
    { kind: "start" },
  );
  assert.deepEqual(
    [deliveryOf(both, codex), deliveryOf(both, claude)],
    [sent, sending],
  );
  assert.deepEqual(deliveryOf(started, codex), sending, "steps never mutate");
});

function member(id: string, alias: `@${string}`): [AddressId, Member] {
  return [id as AddressId, { id: id as AddressId, alias } as unknown as Member];
}
function post(legs: readonly string[]): Message {
  return {
    id: "p1",
    threadId: "p1",
    inReplyTo: null,
    at: 1,
    author: "human",
    kind: "mention",
    body: { text: "nice catch by @codex", truncated: false },
    legs: legs.map((to) => ({
      to: to as AddressId,
      stage: { stage: "delivered" },
    })),
    routing: null,
    woke: [],
  };
}
function channels(message: Message | null) {
  return new Map([
    [
      key,
      {
        members: new Map([
          member("codex", "@codex"),
          member("claude", "@claude"),
        ]),
        messages: new Map(message === null ? [] : [[message.id, message]]),
      },
    ],
  ]);
}

test("deliveries: an offer's state ends once a frame shows the post reached that agent", () => {
  const both: Deliveries = stepDelivery(
    stepDelivery(stepDelivery(noDeliveries, codex, { kind: "start" }), codex, {
      kind: "done",
    }),
    claude,
    { kind: "start" },
  );
  assert.equal(settleDeliveries(both, channels(post([]))), both);
  assert.equal(
    settleDeliveries(both, channels(null)),
    both,
    "a post outside the loaded window keeps its state",
  );
  const reached = settleDeliveries(both, channels(post(["codex"])));
  assert.deepEqual(
    [deliveryOf(reached, codex), deliveryOf(reached, claude)],
    [idle, sending],
  );
  assert.equal(
    stepDelivery(reached, codex, { kind: "done" }),
    reached,
    "a late answer for a settled offer changes nothing",
  );
  assert.equal(
    settleDeliveries(reached, channels(post(["codex", "claude"]))).size,
    0,
  );
});
