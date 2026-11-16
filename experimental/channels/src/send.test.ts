import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
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
  assert.deepEqual(
    await poster.control(
      key,
      { kind: "thread", threadId: "thread 1/a", action: "continue" },
      press,
    ),
    {
      kind: "done",
    },
  );
  assert.deepEqual(
    await poster.control(
      key,
      { kind: "thread", threadId: "t2", action: "stop" },
      press,
    ),
    {
      kind: "done",
    },
  );
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

test("control: a dismiss posts the person's confirmation and the cards' ids", async () => {
  const server = fakeServer(({ url }) =>
    url === "/api/session" ? json(200, { csrf_token: "t" }) : json(200, {}),
  );
  assert.deepEqual(
    await createPoster(server.fetcher).control(
      key,
      {
        kind: "attention",
        ids: ["attention-agent-disposition-attempt 1/a", "agent-status-a1"],
        action: "dismiss",
      },
      press,
    ),
    { kind: "done" },
  );
  assert.deepEqual(
    posts(server.seen).map(({ url, csrf, type, body }) => [
      url,
      csrf,
      type,
      body,
    ]),
    [
      [
        `/w/${key}/attention/dismiss`,
        "t",
        "application/json",
        {
          confirmed: true,
          ids: ["attention-agent-disposition-attempt 1/a", "agent-status-a1"],
        },
      ],
    ],
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
      await createPoster(server.fetcher).control(
        key,
        { kind: "thread", threadId: "t1", action: "stop" },
        press,
      ),
      expected,
    );
  }
});

test("deliver: posts the bare aliases for a stored post with the session token, sharing the 403 retry", async () => {
  let token = 0;
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: `t${++token}` });
    return new Headers(init?.headers).get("X-Namedrop-Csrf") === "t2"
      ? json(200, {
          channel: { state: "unrouted", to: ["codex"] },
          delivered: JSON.parse(String(init?.body)).to,
        })
      : new Response("Forbidden.\n", { status: 403 });
  });
  const poster = createPoster(server.fetcher);
  assert.deepEqual(
    Object.fromEntries(
      (await poster.deliver(key, "post 1/a", ["@codex"])).results,
    ),
    { "@codex": { kind: "done" } },
  );
  assert.deepEqual(
    Object.fromEntries(
      (await poster.deliver(key, "p2", ["@codex", "@claude"])).results,
    ),
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

test("deliver: a 200 that lists only some aliases sends the rest back as failed, with the post's channel object", async () => {
  const server = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "t" })
      : json(200, {
          channel: { state: "unrouted", to: ["codex"] },
          delivered: ["codex"],
        }),
  );
  const answer = await createPoster(server.fetcher).deliver(key, "p1", [
    "@codex",
    "@claude",
  ]);
  assert.deepEqual(Object.fromEntries(answer.results), {
    "@codex": { kind: "done" },
    "@claude": { kind: "failed", cause: "left-out" },
  });
  assert.deepEqual(
    answer.channel,
    { state: "unrouted", mentioned: [], to: ["@codex"] },
    "the post's channel object comes back with the answer",
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
        (await createPoster(server.fetcher, 50).deliver(key, "p1", ["@codex"]))
          .results,
      ),
      { "@codex": { kind: "done" } },
    );
    assert.ok(Date.now() - started < 2_000);
  }
});

test("deliver: a 200 whose body stalls is aborted when the body wait ends, so the server sees its socket close", async () => {
  const wait = 100;
  const margin = 1_000;
  let closed: number | null = null;
  const server = createServer((req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    if (req.url === "/api/session") return res.end('{"csrf_token":"t"}');
    req.socket.once("close", () => {
      closed = Date.now();
    });
    res.write('{"delivered":');
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  const fetcher: typeof fetch = (input, init) =>
    fetch(`http://127.0.0.1:${port}${String(input)}`, init);
  try {
    const started = Date.now();
    assert.deepEqual(
      Object.fromEntries(
        (await createPoster(fetcher, wait).deliver(key, "p1", ["@codex"]))
          .results,
      ),
      { "@codex": { kind: "done" } },
      "the press still reads as sent",
    );
    while (closed === null && Date.now() - started < wait + margin)
      await new Promise((resolve) => setTimeout(resolve, 10));
    const seen: number | null = closed;
    assert.ok(
      seen !== null && seen - started <= wait + margin,
      `the server saw the socket close within ${wait + margin} ms`,
    );
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("deliver: a refusal carries the server's message, a network failure reads status 0, and a 200 that lists no one readable confirms nothing", async () => {
  const refused = (status: number, message: string) => ({
    kind: "failed",
    cause: "refused",
    status,
    message,
  });
  const cases: [Response | Error, unknown][] = [
    [
      json(400, { error: "@shell is not an agent in this workspace." }),
      refused(400, "@shell is not an agent in this workspace."),
    ],
    [new Response("Not found.\n", { status: 404 }), refused(404, "Not found.")],
    [new Response("", { status: 500 }), refused(500, "")],
    [new Error("offline"), refused(0, "")],
    [
      json(200, { channel: { state: "unrouted", to: ["codex"] } }),
      { kind: "failed", cause: "unconfirmed" },
    ],
  ];
  for (const [outcome, expected] of cases) {
    const server = fakeServer(({ url }) => {
      if (url === "/api/session") return json(200, { csrf_token: "t" });
      if (outcome instanceof Error) throw outcome;
      return outcome;
    });
    assert.deepEqual(
      Object.fromEntries(
        (
          await createPoster(server.fetcher).deliver(key, "p1", [
            "@codex",
            "@claude",
          ])
        ).results,
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
  await createPoster(server.fetcher).control(
    key,
    { kind: "thread", threadId: "t1", action: "stop" },
    press.signal,
  );
  assert.deepEqual(
    server.seen.map(({ url, init }) => [url, init?.signal === press.signal]),
    [
      ["/api/session", false],
      [`/w/${key}/threads/t1/stop`, true],
    ],
  );
});

test("fullBody: reads one message with the session token, refreshes a stale token once, and refuses a mismatched answer", async () => {
  let token = "t1";
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: token });
    const csrf = new Headers(init?.headers).get("X-Namedrop-Csrf");
    if (csrf !== "t2") return new Response("Forbidden.\n", { status: 403 });
    const id = decodeURIComponent(url.split("/")[4] ?? "");
    return json(200, {
      message_id: id === "odd" ? "other" : id,
      body: { text: `all of ${id}`, truncated: false },
    });
  });
  const poster = createPoster(server.fetcher);
  poster.prepare();
  token = "t2";
  assert.deepEqual(await poster.fullBody(key, "m/1"), {
    text: "all of m/1",
    truncated: false,
  });
  assert.equal(
    server.seen.find((exchange) => exchange.url.endsWith("/body"))?.url,
    `/w/${key}/messages/m%2F1/body`,
  );
  assert.equal(await poster.fullBody(key, "odd"), null);
  assert.ok(
    server.seen.every((exchange) => exchange.init?.method !== "POST"),
    "reading a body never posts",
  );
});

test("reminder Cancel sends the scoped id with CSRF and retries an expired token", async () => {
  let token = 0;
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session")
      return json(200, { csrf_token: `token-${++token}` });
    return new Headers(init?.headers).get("X-Namedrop-Csrf") === "token-1"
      ? json(403, {})
      : json(200, { state: "cancelled" });
  });
  const poster = createPoster(server.fetcher);
  const result = await poster.cancelReminder(
    { kind: "thread", key, thread: "thread:one" },
    "reminder:one",
    press,
  );
  assert.deepEqual(result, { kind: "done" });
  assert.deepEqual(
    posts(server.seen).map(({ url, csrf }) => ({ url, csrf })),
    [
      {
        url: `/w/${key}/threads/thread%3Aone/reminders/reminder%3Aone/cancel`,
        csrf: "token-1",
      },
      {
        url: `/w/${key}/threads/thread%3Aone/reminders/reminder%3Aone/cancel`,
        csrf: "token-2",
      },
    ],
  );
});

test("uploadImage: posts the file's bytes with the session token, refreshes a stale token once, and says why an upload was refused", async () => {
  const name = `${"a".repeat(64)}.png`;
  const line = `Image: /state/channel-images/${name}`;
  let token = "t1";
  let answer: () => Response = () => json(200, { name, line });
  const server = fakeServer(({ url, init }) => {
    if (url === "/api/session") return json(200, { csrf_token: token });
    const csrf = new Headers(init?.headers).get("X-Namedrop-Csrf");
    return csrf === "t2"
      ? answer()
      : new Response("Forbidden.\n", { status: 403 });
  });
  const poster = createPoster(server.fetcher);
  poster.prepare();
  token = "t2";
  const file = new Blob(["png bytes"], { type: "image/png" });
  assert.deepEqual(await poster.uploadImage(file), {
    kind: "stored",
    image: { name, line },
  });
  const uploads = server.seen.filter(
    (exchange) => exchange.url === "/api/images",
  );
  assert.equal(uploads.length, 2, "one refused try, then the fresh token");
  assert.equal(uploads[1]?.init?.method, "POST");
  assert.equal(uploads[1]?.init?.body, file, "the bytes go up as they are");

  for (const [status, why] of [
    [415, "type"],
    [413, "size"],
    [429, "failed"],
    [503, "failed"],
  ] as const) {
    answer = () => new Response("", { status });
    assert.deepEqual(await poster.uploadImage(file), { kind: "refused", why });
  }
  for (const wrong of [
    { name, line: `Image: /state/channel-images/${"b".repeat(64)}.png` },
    { name, line: `text first\n${line}` },
    { name: "../session", line: "Image: /state/dashboard/session" },
    { name },
  ]) {
    answer = () => json(200, wrong);
    assert.deepEqual(
      await poster.uploadImage(file),
      { kind: "refused", why: "failed" },
      JSON.stringify(wrong),
    );
  }
  const offline = createPoster(async (input) => {
    if (String(input) === "/api/session") return json(200, { csrf_token: "t" });
    throw new TypeError("Failed to fetch");
  });
  assert.deepEqual(await offline.uploadImage(file), {
    kind: "refused",
    why: "failed",
  });
});

test("createChannel: posts the name and folder with the session token and reads the new channel's key", async () => {
  const made = "b".repeat(20);
  const server = fakeServer(({ url }, index) =>
    url === "/api/session"
      ? json(200, { csrf_token: `t${index}` })
      : index === 1
        ? json(403, {})
        : json(201, { key: made, workspace_id: "w7", label: "Proto Chan" }),
  );
  const poster = createPoster(server.fetcher);
  assert.deepEqual(
    await poster.createChannel(key, { name: "Proto Chan", folder: "~/proj" }),
    {
      kind: "created",
      channel: { key: made, workspaceId: "w7", label: "Proto Chan" },
    },
  );
  assert.deepEqual(posts(server.seen), [
    {
      url: `/w/${key}/channels`,
      csrf: "t0",
      type: "application/json",
      body: { name: "Proto Chan", folder: "~/proj" },
    },
    {
      url: `/w/${key}/channels`,
      csrf: "t2",
      type: "application/json",
      body: { name: "Proto Chan", folder: "~/proj" },
    },
  ]);
});

test("addAgent: posts the kind, bare name and persona and reads the new address", async () => {
  const server = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "t1" })
      : json(201, { address_id: "pane-address-9", alias: "reviewer" }),
  );
  const poster = createPoster(server.fetcher);
  assert.deepEqual(
    await poster.addAgent(key, {
      kind: "codex",
      name: "reviewer",
      persona: null,
    }),
    {
      kind: "created",
      agent: { addressId: "pane-address-9", alias: "reviewer" },
    },
  );
  assert.deepEqual(posts(server.seen), [
    {
      url: `/w/${key}/agents`,
      csrf: "t1",
      type: "application/json",
      body: { kind: "codex", name: "reviewer", persona: null },
    },
  ]);
});

test("createChannel and addAgent: a refusal carries the server's sentence and status, and a broken answer reads status 0", async () => {
  const answers = [
    json(409, {
      error: "Another channel is already named #web. Pick a different name.",
    }),
    new Response("Forbidden.\n", { status: 403 }),
    new Response("Forbidden.\n", { status: 403 }),
    json(201, { key: "not-a-key" }),
  ];
  const server = fakeServer(({ url }) =>
    url === "/api/session"
      ? json(200, { csrf_token: "t1" })
      : (answers.shift() ?? json(500, {})),
  );
  const poster = createPoster(server.fetcher);
  const request = { name: "web", folder: "/srv" };
  assert.deepEqual(await poster.createChannel(key, request), {
    kind: "refused",
    status: 409,
    message: "Another channel is already named #web. Pick a different name.",
  });
  assert.deepEqual(await poster.createChannel(key, request), {
    kind: "refused",
    status: 403,
    message: "Forbidden.",
  });
  assert.deepEqual(await poster.createChannel(key, request), {
    kind: "refused",
    status: 0,
    message: "",
  });
  const offline = createPoster(async () => {
    throw new TypeError("Failed to fetch");
  });
  assert.deepEqual(
    await offline.addAgent(key, { kind: "claude", name: "x", persona: null }),
    { kind: "refused", status: 0, message: "" },
  );
});

test("folders: reads the recent folders, and any failure is an empty list", async () => {
  const answers = [
    json(200, { folders: ["/home/x/proj", "", 7, "/srv/web"] }),
    json(503, {
      error:
        "Herdr isn't reachable. Check that Herdr is running, then try again.",
    }),
    json(200, { nope: true }),
  ];
  const server = fakeServer(() => answers.shift() ?? json(404, {}));
  const poster = createPoster(server.fetcher);
  assert.deepEqual(await poster.folders(key, press), [
    "/home/x/proj",
    "/srv/web",
  ]);
  assert.deepEqual(await poster.folders(key, press), []);
  assert.deepEqual(await poster.folders(key, press), []);
  assert.equal(server.seen[0]?.url, `/w/${key}/folders`);
});

test("channelFolder: reads where the next agent opens, and any failure is no folder", async () => {
  const answers = [
    json(200, { folder: "/home/x/proj" }),
    json(200, { folder: null }),
    json(404, { error: "This channel's workspace is closed." }),
    json(200, { folder: 7 }),
  ];
  const server = fakeServer(() => answers.shift() ?? json(404, {}));
  const poster = createPoster(server.fetcher);
  assert.equal(await poster.channelFolder(key, press), "/home/x/proj");
  assert.equal(await poster.channelFolder(key, press), null);
  assert.equal(await poster.channelFolder(key, press), null);
  assert.equal(await poster.channelFolder(key, press), null);
  assert.equal(server.seen[0]?.url, `/w/${key}/folder`);
  assert.equal(server.seen[0]?.init?.method ?? "GET", "GET");
  assert.equal(server.seen[0]?.init?.credentials, "same-origin");
  assert.equal(server.seen[0]?.init?.method, undefined);
});
