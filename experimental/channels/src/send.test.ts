import assert from "node:assert/strict";
import { test } from "node:test";
import type { SpaceKey } from "./model.ts";
import { createPoster, type Post } from "./send.ts";
import { routeOf } from "./wire.ts";

type Exchange = { url: string; init: RequestInit | undefined };
function fakeServer(
  respond: (exchange: Exchange, index: number) => Response | Promise<Response>,
) {
  const seen: Exchange[] = [];
  const fetcher: typeof fetch = async (input, init) => {
    const exchange = { url: String(input), init };
    seen.push(exchange);
    return respond(exchange, seen.length - 1);
  };
  return { seen, fetcher };
}
const route = routeOf({ pathname: `/w/${"a".repeat(20)}`, search: "" });
assert.ok(route.kind === "channel");
const key: SpaceKey = route.key;
const post: Post = {
  key,
  requestId: "r1",
  source: "@codex hi",
  inReplyTo: null,
};
const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
function posts(seen: Exchange[]) {
  return seen
    .filter((exchange) => exchange.init?.method === "POST")
    .map((exchange) => ({
      url: exchange.url,
      csrf: new Headers(exchange.init?.headers).get("X-Namedrop-Csrf"),
      type: new Headers(exchange.init?.headers).get("Content-Type"),
      body: JSON.parse(String(exchange.init?.body)),
    }));
}

test("post: sends the token from the session route and returns the receipt's message id", async () => {
  const server = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "t1" })
      : json(200, { message_id: "m9", thread_id: "m9" }),
  );
  const poster = createPoster(server.fetcher);
  assert.deepEqual(await poster.post(post), { kind: "sent", messageId: "m9" });
  assert.deepEqual(
    await poster.post({ ...post, requestId: "r2", inReplyTo: "m9" }),
    { kind: "sent", messageId: "m9" },
  );
  assert.deepEqual(posts(server.seen), [
    {
      url: `/w/${key}/messages`,
      csrf: "t1",
      type: "application/json",
      body: { request_id: "r1", source: "@codex hi" },
    },
    {
      url: `/w/${key}/messages`,
      csrf: "t1",
      type: "application/json",
      body: { request_id: "r2", source: "@codex hi", in_reply_to: "m9" },
    },
  ]);
  assert.equal(
    server.seen.filter(({ url }) => url === "/api/session").length,
    1,
    "the token is fetched once",
  );
});

test("post: a 403 refreshes the token once and retries the same request", async () => {
  let token = 0;
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: `t${++token}` });
    return new Headers(init?.headers).get("X-Namedrop-Csrf") === "t2"
      ? json(200, { message_id: "m1" })
      : new Response("Forbidden.\n", { status: 403 });
  });
  assert.deepEqual(await createPoster(server.fetcher).post(post), {
    kind: "sent",
    messageId: "m1",
  });
  const sent = posts(server.seen);
  assert.deepEqual(
    sent.map(({ csrf, body }) => [csrf, body.request_id]),
    [
      ["t1", "r1"],
      ["t2", "r1"],
    ],
  );
  const refused = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "stale" })
      : new Response("Forbidden.\n", { status: 403 }),
  );
  assert.deepEqual(await createPoster(refused.fetcher).post(post), {
    kind: "failed",
    status: 403,
    message: "Forbidden.",
  });
  assert.equal(posts(refused.seen).length, 2, "only one retry");
});

test("post: refusals carry the server's message and failures carry their status", async () => {
  const cases: [Response | Error, unknown][] = [
    [
      json(400, { error: "Unknown recipient @ghost" }),
      { kind: "failed", status: 400, message: "Unknown recipient @ghost" },
    ],
    [
      new Response("Not found.\n", { status: 404 }),
      { kind: "failed", status: 404, message: "Not found." },
    ],
    [
      json(503, { error: "Namedrop is unavailable." }),
      { kind: "failed", status: 503, message: "Namedrop is unavailable." },
    ],
    [
      json(200, { thread_id: "t" }),
      { kind: "failed", status: 200, message: "" },
    ],
    [new Error("offline"), { kind: "failed", status: 0, message: "" }],
  ];
  for (const [outcome, expected] of cases) {
    const server = fakeServer(({ url }) => {
      if (url === "/api/session") return json(200, { csrf_token: "t" });
      if (outcome instanceof Error) throw outcome;
      return outcome;
    });
    assert.deepEqual(await createPoster(server.fetcher).post(post), expected);
  }
});
