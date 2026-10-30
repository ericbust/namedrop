import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createDeliveries,
  type Delivery,
  delivery,
  deliveryOf,
  type Offer,
} from "./delivery.ts";
import type {
  AddressId,
  Alias,
  DeliverResult,
  Member,
  Message,
  SpaceKey,
} from "./model.ts";
import { parseDelivered } from "./wire.ts";

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
const codex: Offer = { key, messageId: "p1", alias: "@codex" };
const claude: Offer = { key, messageId: "p1", alias: "@claude" };
const settled = () => new Promise((resolve) => setImmediate(resolve));
function server() {
  const requests: Offer[] = [];
  const answers = new Map<
    string,
    (results: ReadonlyMap<Alias, DeliverResult>) => void
  >();
  const store = createDeliveries(
    (offer) =>
      new Promise((resolve) => {
        requests.push(offer);
        answers.set(offer.alias, resolve);
      }),
  );
  const answer = async (
    offer: Offer,
    result: DeliverResult,
    others: readonly [Alias, DeliverResult][] = [],
  ) => {
    answers.get(offer.alias)?.(new Map([...others, [offer.alias, result]]));
    await settled();
  };
  const of = (offer: Offer) => deliveryOf(store.snapshot(), offer);
  return { store, requests, answer, of };
}

test("deliveries: a press sends once, each offer keeps its own state, and Try again sends again", async () => {
  const { store, requests, answer, of } = server();
  let changes = 0;
  store.subscribe(() => changes++);
  store.deliver(codex);
  store.deliver(codex);
  assert.deepEqual(
    requests,
    [codex],
    "a second press while sending sends nothing",
  );
  assert.equal(changes, 1);
  assert.deepEqual([of(codex), of(claude)], [sending, idle]);
  store.deliver(claude);
  await answer(codex, { kind: "done" });
  assert.deepEqual([of(codex), of(claude)], [sent, sending]);
  store.deliver(codex);
  assert.equal(requests.length, 2, "Sent never sends again");
  await answer(claude, {
    kind: "failed",
    status: 400,
    message: "@codex is not an agent here.",
  });
  assert.deepEqual(of(claude), refused);
  store.deliver(claude);
  assert.deepEqual(requests, [codex, claude, claude]);
  assert.deepEqual(of(claude), sending);
});

test("deliveries: an answer moves each agent it names that was sending, and no other", async () => {
  const { store, requests, answer, of } = server();
  store.deliver(codex);
  await answer(codex, { kind: "done" }, [
    ["@claude", { kind: "failed", status: 200, message: "" }],
  ]);
  assert.deepEqual(
    [of(codex), of(claude)],
    [sent, idle],
    "@claude was never pressed",
  );
  store.deliver(claude);
  store.deliver(codex);
  await answer(claude, { kind: "failed", status: 200, message: "" }, [
    ["@codex", { kind: "failed", status: 200, message: "" }],
  ]);
  assert.deepEqual(
    [of(codex), of(claude)],
    [sent, { kind: "failed", status: 200, message: "" }],
    "Sent stays sent",
  );
  assert.deepEqual(requests, [codex, claude]);
});

test("deliveries: a 200 leaves the pressed agent Sent only when it lists the agent, and otherwise failed with the server's reason", async () => {
  const notSent = (message: string): Delivery => ({
    kind: "failed",
    status: 200,
    message,
  });
  const reason = "@codex belongs to a different agent now.";
  const cases: [string, Delivery][] = [
    ['{"channel":{},"delivered":["codex"]}', sent],
    ['{"channel":{},"delivered":["claude"]}', notSent("")],
    ['{"delivered":[]}', notSent("")],
    [
      JSON.stringify({
        delivered: [],
        not_delivered: [{ alias: "codex", reason }],
        error: "Namedrop skipped 1 agent.",
      }),
      notSent(reason),
    ],
    [
      '{"delivered":[],"error":"Namedrop skipped 1 agent."}',
      notSent("Namedrop skipped 1 agent."),
    ],
    ['{"channel":{}}', sent],
    ['{"delivered":"codex"}', sent],
    ["Sent.", sent],
  ];
  for (const [body, expected] of cases) {
    const store = createDeliveries(async ({ alias }) =>
      parseDelivered(body, [alias]),
    );
    store.deliver(codex);
    await settled();
    assert.deepEqual(deliveryOf(store.snapshot(), codex), expected, body);
  }
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

test("deliveries: an offer's state ends once a frame shows the post reached that agent", async () => {
  const { store, answer, of } = server();
  store.deliver(codex);
  store.deliver(claude);
  await answer(claude, { kind: "done" });
  const before = store.snapshot();
  store.settle(channels(post([])));
  store.settle(channels(null));
  assert.equal(
    store.snapshot(),
    before,
    "a post that reached no one, or one outside the loaded window, keeps its states",
  );
  store.settle(channels(post(["codex"])));
  assert.deepEqual([of(codex), of(claude)], [idle, sent]);
  await answer(codex, { kind: "done" });
  assert.deepEqual(
    of(codex),
    idle,
    "a late answer for a settled offer changes nothing",
  );
  store.settle(channels(post(["codex", "claude"])));
  assert.equal(store.snapshot().size, 0);
});
