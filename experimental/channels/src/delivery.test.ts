import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type After,
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
import { parseDeliverAnswer } from "./wire.ts";

const idle: Delivery = { kind: "idle" };
const sending: Delivery = { kind: "sending" };
const sent: Delivery = { kind: "sent" };
const refused: Delivery = {
  kind: "failed",
  cause: "refused",
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
      cause: "refused",
      status: 400,
      message: "@codex is not an agent here.",
    }),
    refused,
  );
  for (const state of [idle, sent, refused]) {
    assert.equal(delivery(state, { kind: "done" }), state);
    assert.equal(
      delivery(state, {
        kind: "failed",
        cause: "refused",
        status: 0,
        message: "",
      }),
      state,
    );
  }
});

test("delivery: a frame that shows the agent reached confirms a press still sending, ends a failed one, and ends a sent one only once its hold is over", () => {
  for (const held of [true, false])
    assert.deepEqual(delivery(sending, { kind: "reached", held }), sent);
  assert.equal(
    delivery(sent, { kind: "reached", held: true }),
    sent,
    "Sent holds",
  );
  assert.deepEqual(delivery(sent, { kind: "reached", held: false }), idle);
  assert.deepEqual(delivery(refused, { kind: "reached", held: false }), idle);
  assert.equal(delivery(idle, { kind: "reached", held: false }), idle);
});

const key = "a".repeat(20) as SpaceKey;
const codex: Offer = { key, messageId: "p1", alias: "@codex" };
const claude: Offer = { key, messageId: "p1", alias: "@claude" };
const settled = () => new Promise((resolve) => setImmediate(resolve));
// A clock the test moves by hand.
function fakeClock() {
  let now = 0;
  const due = new Set<{ at: number; run: () => void }>();
  const after: After = (ms, run) => {
    const timer = { at: now + ms, run };
    due.add(timer);
    return () => {
      due.delete(timer);
    };
  };
  const tick = (ms: number) => {
    now += ms;
    for (const timer of [...due].sort((a, b) => a.at - b.at))
      if (timer.at <= now && due.delete(timer)) timer.run();
  };
  return { after, tick };
}
function server(wait?: After) {
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
    wait,
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
    cause: "refused",
    status: 400,
    message: "@codex is not an agent here.",
  });
  assert.deepEqual(of(claude), refused);
  store.deliver(claude);
  assert.deepEqual(requests, [codex, claude, claude]);
  assert.deepEqual(of(claude), sending);
});

test("deliveries: the latest change names the agent and its new state, for the page to announce", async () => {
  const { store, answer } = server();
  assert.equal(store.latest(), null);
  store.deliver(codex);
  assert.deepEqual(store.latest(), { offer: codex, state: sending });
  store.deliver(claude);
  assert.deepEqual(store.latest(), { offer: claude, state: sending });
  await answer(codex, { kind: "done" });
  assert.deepEqual(store.latest(), { offer: codex, state: sent });
  const latest = store.latest();
  store.deliver(codex);
  store.settle(channels(post(["codex"])));
  assert.equal(
    store.latest(),
    latest,
    "a press that sends nothing and a frame that ends an offer announce nothing",
  );
  await answer(claude, {
    kind: "failed",
    cause: "refused",
    status: 400,
    message: "@codex is not an agent here.",
  });
  assert.deepEqual(store.latest(), { offer: claude, state: refused });
});

test("deliveries: an answer moves each agent it names that was sending, and no other", async () => {
  const { store, requests, answer, of } = server();
  store.deliver(codex);
  await answer(codex, { kind: "done" }, [
    ["@claude", { kind: "failed", cause: "left-out" }],
  ]);
  assert.deepEqual(
    [of(codex), of(claude)],
    [sent, idle],
    "@claude was never pressed",
  );
  store.deliver(claude);
  store.deliver(codex);
  await answer(claude, { kind: "failed", cause: "left-out" }, [
    ["@codex", { kind: "failed", cause: "left-out" }],
  ]);
  assert.deepEqual(
    [of(codex), of(claude)],
    [sent, { kind: "failed", cause: "left-out" }],
    "Sent stays sent",
  );
  assert.deepEqual(requests, [codex, claude]);
});

test("deliveries: a 200 leaves the pressed agent Sent only when it lists the agent, failed when it leaves the agent out or lists no one readable, and Sent when the body is not JSON", async () => {
  const leftOut: Delivery = { kind: "failed", cause: "left-out" };
  const unconfirmed: Delivery = { kind: "failed", cause: "unconfirmed" };
  const cases: [string, Delivery][] = [
    ['{"channel":{},"delivered":["codex"]}', sent],
    ['{"channel":{},"delivered":["claude"]}', leftOut],
    ['{"delivered":[]}', leftOut],
    ['{"channel":{}}', unconfirmed],
    ['{"delivered":"codex"}', unconfirmed],
    ["Sent.", sent],
    ["", sent],
  ];
  for (const [body, expected] of cases) {
    const store = createDeliveries(
      async ({ alias }) => parseDeliverAnswer(body, [alias]).results,
    );
    store.deliver(codex);
    await settled();
    assert.deepEqual(deliveryOf(store.snapshot(), codex), expected, body);
  }
});

function member(id: string, alias: `@${string}`): [AddressId, Member] {
  return [id as AddressId, { id: id as AddressId, alias } as unknown as Member];
}
// The post as a frame shows it: `legs` once the server delivered, and `to`,
// as a deliver answer's channel object also does, once it queued the delivery.
function post(legs: readonly string[], to: readonly Alias[] = []): Message {
  return {
    id: "p1",
    threadId: "p1",
    inReplyTo: null,
    at: 1,
    author: "human",
    kind: "mention",
    body: { text: "nice catch by @codex and @claude", truncated: false },
    legs: legs.map((to) => ({
      to: to as AddressId,
      stage: { stage: "delivered" },
    })),
    routing: { state: "unrouted", mentioned: ["@codex", "@claude"], to },
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

// What the page's announcer says, in order, as the latest change moves.
function announcements(store: ReturnType<typeof createDeliveries>) {
  const said: string[] = [];
  let last = store.latest();
  store.subscribe(() => {
    const latest = store.latest();
    if (latest !== last && latest !== null)
      said.push(`${latest.state.kind} ${latest.offer.alias}`);
    last = latest;
  });
  return said;
}
const outage: DeliverResult = {
  kind: "failed",
  cause: "refused",
  status: 503,
  message: "",
};

test("deliveries: a post that reached no one, or one outside the loaded window, keeps every press as it is", async () => {
  const { store, answer } = server();
  store.deliver(codex);
  store.deliver(claude);
  await answer(claude, { kind: "done" });
  const before = store.snapshot();
  store.settle(channels(post([])));
  store.settle(channels(null));
  assert.equal(store.snapshot(), before);
});

test("deliveries: a frame that reaches the agent before the answer confirms the press, which reads Sent for 1.5 s and then ends, and the answer after it changes nothing", async () => {
  const clock = fakeClock();
  const { store, answer, of } = server(clock.after);
  const said = announcements(store);
  store.deliver(codex);
  store.settle(channels(post(["codex"])));
  assert.deepEqual(of(codex), sent, "the frame confirms the press");
  clock.tick(1_499);
  assert.deepEqual(of(codex), sent);
  await answer(codex, outage);
  assert.deepEqual(
    of(codex),
    sent,
    "a failure after the frame changes nothing",
  );
  clock.tick(1);
  assert.deepEqual(of(codex), idle, "the hold is over");
  await answer(codex, { kind: "done" });
  assert.deepEqual(of(codex), idle, "an answer after the hold changes nothing");
  assert.deepEqual(said, ["sending @codex", "sent @codex"]);
});

test("deliveries: an answer that lists the agent confirms the press, which reads Sent for 1.5 s while the line already names the agent, then ends", async () => {
  const clock = fakeClock();
  const { store, answer, of } = server(clock.after);
  const said = announcements(store);
  store.deliver(codex);
  await answer(codex, { kind: "done" });
  store.settle(channels(post([], ["@codex"])));
  assert.deepEqual(of(codex), sent, "the channel object does not end it");
  clock.tick(1_000);
  store.settle(channels(post(["codex"], ["@codex"])));
  assert.deepEqual(of(codex), sent, "nor does the frame after it");
  clock.tick(499);
  assert.deepEqual(of(codex), sent);
  clock.tick(1);
  assert.equal(store.snapshot().size, 0, "the hold is over");
  assert.deepEqual(said, ["sending @codex", "sent @codex"]);
});

test("deliveries: a Sent press the line does not name yet keeps reading Sent after its hold, and ends when a frame names the agent", async () => {
  const clock = fakeClock();
  const { store, answer, of } = server(clock.after);
  store.deliver(codex);
  await answer(codex, { kind: "done" });
  store.settle(channels(post([])));
  clock.tick(1_500);
  assert.deepEqual(of(codex), sent, "never offered again before a frame");
  store.settle(channels(post(["codex"])));
  assert.deepEqual(of(codex), idle);
});

test("deliveries: a frame that reaches an agent whose press failed ends that press at once", async () => {
  const { store, answer, of } = server();
  store.deliver(codex);
  await answer(codex, outage);
  store.settle(channels(post(["codex"])));
  assert.deepEqual(of(codex), idle);
});

test("deliveries: each press keeps its own hold, pressing Sent neither sends nor restarts it, and dispose stops every hold", async () => {
  const clock = fakeClock();
  const { store, requests, answer, of } = server(clock.after);
  store.deliver(codex);
  await answer(codex, { kind: "done" });
  clock.tick(1_000);
  store.deliver(claude);
  store.settle(channels(post(["codex", "claude"])));
  store.deliver(codex);
  assert.deepEqual(requests, [codex, claude], "Sent never sends again");
  clock.tick(500);
  assert.deepEqual(
    [of(codex), of(claude)],
    [idle, sent],
    "@codex's hold started at its answer, @claude's at the frame",
  );
  store.dispose();
  clock.tick(1_500);
  assert.deepEqual(of(claude), sent, "no hold runs after dispose");
});
