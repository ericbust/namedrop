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
const press = new AbortController().signal;
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

test("control: posts a thread action with the session token and an empty body, sharing the token and the 403 retry with messages", async () => {
  let token = 0;
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: `t${++token}` });
    if (new Headers(init?.headers).get("X-Namedrop-Csrf") !== "t2")
      return new Response("Forbidden.\n", { status: 403 });
    return url.endsWith("/messages")
      ? json(200, { message_id: "m1" })
      : json(200, {});
  });
  const poster = createPoster(server.fetcher);
  assert.deepEqual(await poster.control(key, "thread 1/a", "continue", press), {
    kind: "done",
  });
  assert.deepEqual(await poster.control(key, "t2", "stop", press), {
    kind: "done",
  });
  assert.deepEqual(await poster.post(post), { kind: "sent", messageId: "m1" });
  assert.deepEqual(
    posts(server.seen).map(({ url, csrf, type, body }) => [
      url,
      csrf,
      type,
      body,
    ]),
    [
      [
        `/w/${key}/threads/thread%201%2Fa/continue`,
        "t1",
        "application/json",
        {},
      ],
      [
        `/w/${key}/threads/thread%201%2Fa/continue`,
        "t2",
        "application/json",
        {},
      ],
      [`/w/${key}/threads/t2/stop`, "t2", "application/json", {}],
      [
        `/w/${key}/messages`,
        "t2",
        "application/json",
        { request_id: "r1", source: "@codex hi" },
      ],
    ],
  );
  assert.equal(
    server.seen.filter(({ url }) => url === "/api/session").length,
    2,
    "one token, refreshed once",
  );
});

test("control: a refusal or a network failure carries its status", async () => {
  const cases: [Response | Error, unknown][] = [
    [
      new Response("Not found.\n", { status: 404 }),
      { kind: "failed", status: 404 },
    ],
    [json(500, { error: "Broken." }), { kind: "failed", status: 500 }],
    [new Error("offline"), { kind: "failed", status: 0 }],
  ];
  for (const [outcome, expected] of cases) {
    const server = fakeServer(({ url }) => {
      if (url === "/api/session") return json(200, { csrf_token: "t" });
      if (outcome instanceof Error) throw outcome;
      return outcome;
    });
    assert.deepEqual(
      await createPoster(server.fetcher).control(key, "t1", "stop", press),
      expected,
    );
  }
});

test("deliver: posts the bare aliases for a stored post with the session token, sharing the 403 retry", async () => {
  let token = 0;
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: `t${++token}` });
    return new Headers(init?.headers).get("X-Namedrop-Csrf") === "t2"
      ? json(200, { channel: { state: "unrouted", to: ["codex"] } })
      : new Response("Forbidden.\n", { status: 403 });
  });
  const poster = createPoster(server.fetcher);
  assert.deepEqual(
    Object.fromEntries(await poster.deliver(key, "post 1/a", ["@codex"])),
    { "@codex": { kind: "done" } },
  );
  assert.deepEqual(
    Object.fromEntries(await poster.deliver(key, "p2", ["@codex", "@claude"])),
    { "@codex": { kind: "done" }, "@claude": { kind: "done" } },
  );
  assert.deepEqual(posts(server.seen), [
    {
      url: `/w/${key}/messages/post%201%2Fa/deliver`,
      csrf: "t1",
      type: "application/json",
      body: { to: ["codex"] },
    },
    {
      url: `/w/${key}/messages/post%201%2Fa/deliver`,
      csrf: "t2",
      type: "application/json",
      body: { to: ["codex"] },
    },
    {
      url: `/w/${key}/messages/p2/deliver`,
      csrf: "t2",
      type: "application/json",
      body: { to: ["codex", "claude"] },
    },
  ]);
});

test("deliver: a 200 that lists only some aliases sends the rest back as failed with the server's reason", async () => {
  const reason = "@claude belongs to a different agent now.";
  const server = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "t" })
      : json(200, {
          channel: { state: "unrouted", to: ["codex"] },
          delivered: ["codex"],
          not_delivered: [{ alias: "claude", reason }],
        }),
  );
  assert.deepEqual(
    Object.fromEntries(
      await createPoster(server.fetcher).deliver(key, "p1", [
        "@codex",
        "@claude",
      ]),
    ),
    {
      "@codex": { kind: "done" },
      "@claude": { kind: "failed", status: 200, message: reason },
    },
  );
});

test("deliver: a 200 whose body never finishes or breaks still reads as sent, within the body wait", async () => {
  const bodies = [
    new ReadableStream({ start() {} }),
    new ReadableStream({
      start(controller) {
        controller.error(new Error("cut"));
      },
    }),
  ];
  for (const body of bodies) {
    const server = fakeServer(({ url }) =>
      url === "/api/session"
        ? json(200, { csrf_token: "t" })
        : new Response(body, { status: 200 }),
    );
    const started = Date.now();
    assert.deepEqual(
      Object.fromEntries(
        await createPoster(server.fetcher, 50).deliver(key, "p1", ["@codex"]),
      ),
      { "@codex": { kind: "done" } },
    );
    assert.ok(Date.now() - started < 2_000);
  }
});

test("deliver: a refusal carries the server's message and a network failure reads status 0", async () => {
  const cases: [Response | Error, unknown][] = [
    [
      json(400, { error: "@shell is not an agent in this workspace." }),
      {
        kind: "failed",
        status: 400,
        message: "@shell is not an agent in this workspace.",
      },
    ],
    [
      new Response("Not found.\n", { status: 404 }),
      { kind: "failed", status: 404, message: "Not found." },
    ],
    [
      new Response("", { status: 500 }),
      { kind: "failed", status: 500, message: "" },
    ],
    [new Error("offline"), { kind: "failed", status: 0, message: "" }],
  ];
  for (const [outcome, expected] of cases) {
    const server = fakeServer(({ url }) => {
      if (url === "/api/session") return json(200, { csrf_token: "t" });
      if (outcome instanceof Error) throw outcome;
      return outcome;
    });
    assert.deepEqual(
      Object.fromEntries(
        await createPoster(server.fetcher).deliver(key, "p1", [
          "@codex",
          "@claude",
        ]),
      ),
      { "@codex": expected, "@claude": expected },
    );
  }
});

test("control: the thread action's request carries the caller's abort signal", async () => {
  const server = fakeServer(({ url }) =>
    url === "/api/session" ? json(200, { csrf_token: "t" }) : json(200, {}),
  );
  const press = new AbortController();
  await createPoster(server.fetcher).control(key, "t1", "stop", press.signal);
  assert.deepEqual(
    server.seen.map(({ url, init }) => [url, init?.signal === press.signal]),
    [
      ["/api/session", false],
      [`/w/${key}/threads/t1/stop`, true],
    ],
  );
});
