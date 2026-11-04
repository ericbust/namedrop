import assert from "node:assert/strict";
import { test } from "node:test";
import { composerHint, postSource, skillRecipients } from "./complete.ts";
import {
  directComposer,
  directKey,
  directMember,
  directPath,
  directPeers,
  mergeDirectHistory,
} from "./direct-messages.ts";
import { createPoster } from "./send.ts";
import { draftKey } from "./store.ts";
import { parseDirectHistory, parseDirectPeers } from "./wire.ts";

const rawPeer = (space_key = "a".repeat(20), address_id = "pane-1") => ({
  space_key,
  address_id,
  alias: "claude",
  workspace_label: "alpha",
  pane_label: { text: "Builder", truncated: false },
  runtime: "claude-code",
  presence: "idle",
  closed: false,
  thread_id: null,
  last_activity_ms: null,
  last_message_id: null,
  last_agent_message_id: null,
  attention_count: 0,
});
function peer(raw: unknown = rawPeer()) {
  const [peer] = parseDirectPeers({ peers: [raw] });
  assert.ok(peer);
  return peer;
}
test("direct peers validate and normalize aliases at the wire boundary", () => {
  assert.equal(peer().alias, "@claude");
  assert.equal(peer({ ...rawPeer(), alias: "@claude" }).alias, "@claude");
  for (const extra of [
    { presence: "online" },
    { closed: 1 },
    { attention_count: -1 },
    { space_key: "invalid" },
  ])
    assert.throws(() => peer({ ...rawPeer(), ...extra }));
  assert.throws(() => parseDirectPeers({ peers: [rawPeer(), rawPeer()] }));
});
test("duplicate aliases, renames and workspaces never share drafts or endpoints", () => {
  const a = peer();
  const b = peer(rawPeer("b".repeat(20), "pane-1"));
  const renamed = { ...a, alias: "@renamed" };
  assert.notEqual(directKey(a), directKey(b));
  assert.notEqual(draftKey(directComposer(a)), draftKey(directComposer(b)));
  assert.equal(directKey(a), directKey(renamed));
  assert.equal(
    directPath(a),
    `/w/${a.spaceKey}/direct-messages/pane-1/messages`,
  );
  assert.equal(directMember(a).id, a.addressId);
});
test("directory filters aliases, workspace, pane and runtime and orders by private activity", () => {
  const a = peer();
  const b = {
    ...peer(rawPeer("b".repeat(20), "pane-2")),
    workspaceLabel: "beta",
    lastActivityMs: 100,
  };
  assert.deepEqual(directPeers([a, b], ""), [b, a]);
  assert.deepEqual(directPeers([a, b], "claude beta builder"), [b]);
  assert.deepEqual(directPeers([a], "codex"), []);
});
test("DM body mentions and channel names never change the fixed recipient or skill target", () => {
  const a = peer();
  const rule = { kind: "direct", alias: a.alias } satisfies Parameters<
    typeof postSource
  >[1];
  assert.equal(
    postSource(" @other #beta /review ", rule),
    "@other #beta /review",
  );
  assert.equal(postSource("@", rule), "@");
  assert.equal(postSource(" ", rule), null);
  assert.equal(
    composerHint(rule, "@other"),
    "Only @claude receives this direct message.",
  );
  const target = { key: a.spaceKey, member: a.addressId };
  assert.deepEqual(
    skillRecipients("@other /review", {
      current: a.spaceKey,
      channels: [],
      membersOf: () => [],
      directRecipient: target,
    }),
    [target],
  );
});
test("DM history keeps earlier pages and updates receipts without duplicating messages", () => {
  const message = (id: string, at: number) => ({
    message_id: id,
    thread_id: "dm",
    in_reply_to: null,
    at_ms: at,
    author: "human",
    kind: "mention",
    body: { text: id, truncated: false },
    legs: [],
  });
  const newest = parseDirectHistory({
    thread_id: "dm",
    messages: [message("new", 2)],
    earlier_omitted: true,
  });
  const older = parseDirectHistory({
    thread_id: "dm",
    messages: [message("old", 1)],
    earlier_omitted: false,
  });
  const merged = mergeDirectHistory(newest, older);
  assert.deepEqual(
    merged.messages.map((message) => message.id),
    ["old", "new"],
  );
  assert.equal(merged.earlierOmitted, false);
  assert.deepEqual(mergeDirectHistory(merged, newest), merged);
});
test("DM sender uses existing session and CSRF refresh with only the private endpoint body", async () => {
  const a = peer();
  const seen: { path: string; init?: RequestInit }[] = [];
  let token = 0;
  const fetcher: typeof fetch = async (path, init) => {
    seen.push({ path: String(path), init });
    if (path === "/api/session")
      return Response.json({ csrf_token: `t${++token}` });
    if (new Headers(init?.headers).get("X-Namedrop-Csrf") === "t1")
      return new Response("Forbidden", { status: 403 });
    return Response.json({ message_id: "private-1" });
  };
  const result = await createPoster(fetcher).post({
    key: a.spaceKey,
    directTo: a.addressId,
    requestId: "same-request",
    source: "@other secret",
    inReplyTo: null,
  });
  assert.deepEqual(result, { kind: "sent", messageId: "private-1" });
  const posts = seen.filter((item) => item.init?.method === "POST");
  assert.equal(posts.length, 2);
  for (const post of posts) {
    assert.equal(post.path, directPath(a));
    assert.equal(post.init?.credentials, "same-origin");
    assert.deepEqual(JSON.parse(String(post.init?.body)), {
      request_id: "same-request",
      source: "@other secret",
    });
  }
});
