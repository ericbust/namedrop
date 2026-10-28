import assert from "node:assert/strict";
import { test } from "node:test";
import type { Activity, Channel, State } from "./model.ts";
import {
  channelName,
  connectionStatus,
  initialState,
  reduce,
  sortedChannels,
} from "./store.ts";
import { parseEvent } from "./wire.ts";

const keyA = "a".repeat(20);
const keyB = "b".repeat(20);
function participant(id = "agent-1", activity: Activity = "idle") {
  return {
    address_id: id,
    alias: "@codex",
    kind: { type: "agent", runtime: "codex" },
    activity,
    role: null,
    pane_label: null,
  };
}
function snapshot(key = keyA, name = "alpha", workspace = "w1") {
  return {
    key,
    frame: {
      wire: 1,
      generated_at_ms: 100,
      space: {
        key,
        workspace_id: workspace,
        display_name: name,
        addressable_label: name,
      },
      health: { level: "calm" },
      participants: [participant()],
      messages: [{ message_id: "m1" }],
      attention: [],
    },
  };
}
function parsed(value: unknown): Channel {
  const event = parseEvent("space", JSON.stringify(value));
  assert.ok(
    event && event.type === "space",
    "fixture must parse into a space event",
  );
  return event.channel;
}
function apply(state: State, value: unknown): State {
  const event = parseEvent("space", JSON.stringify(value));
  return event ? reduce(state, event) : state;
}
function twoChannels() {
  return apply(apply(initialState(), snapshot()), snapshot(keyB, "beta", "w2"));
}
function changedChannel(activity: Activity = "working") {
  const value = snapshot(keyB, "beta", "w2");
  value.frame.participants = [participant("agent-1", activity)];
  return value;
}
test("space installs a complete channel, selects the first arrival, and leaves the input untouched", () => {
  const before = initialState();
  const channel = parsed(snapshot());
  const after = reduce(before, { type: "space", channel });
  assert.equal(after.channels.get(channel.key), channel);
  assert.equal(after.selectedKey, channel.key);
  assert.equal(after.unread.size, 0);
  assert.equal(before.channels.size, 0);
});
test("semantic replay is an identity no-op despite timestamps, unused fields, and message order", () => {
  const value = snapshot();
  value.frame.messages.push({ message_id: "m2" });
  const state = apply(initialState(), value);
  const replay = {
    ...value,
    ignored: true,
    frame: {
      ...value.frame,
      generated_at_ms: 900,
      messages: [...value.frame.messages].reverse(),
      threads: ["unused"],
    },
  };
  assert.equal(apply(state, replay), state);
});
test("whole snapshots replace members and messages without retaining old IDs", () => {
  const state = apply(initialState(), snapshot());
  const value = snapshot();
  value.frame.messages = [];
  value.frame.participants = [];
  const next = apply(state, value);
  assert.equal([...next.channels.values()][0]?.messages.size, 0);
  assert.equal([...next.channels.values()][0]?.members.size, 0);
  assert.equal([...state.channels.values()][0]?.messages.size, 1);
});
test("gone removes channel and unread metadata, clears selected key, and unknown gone is a no-op", () => {
  const state = apply(twoChannels(), changedChannel());
  const channel = parsed(snapshot(keyB, "beta", "w2"));
  assert.ok(state.unread.has(channel.key));
  const next = reduce(state, { type: "gone", key: channel.key });
  assert.equal(next.channels.size, 1);
  assert.equal(next.unread.size, 0);
  assert.equal(reduce(next, { type: "gone", key: channel.key }), next);
  const selected = reduce(state, { type: "select", key: channel.key });
  assert.equal(
    reduce(selected, { type: "gone", key: channel.key }).selectedKey,
    null,
  );
});
test("same-name channels keep separate keys and each shows its full workspace suffix", () => {
  const state = apply(
    apply(initialState(), snapshot(keyB, "shared", "w22")),
    snapshot(keyA, "shared", "w11"),
  );
  const channels = sortedChannels(state);
  assert.equal(state.channels.size, 2);
  assert.deepEqual(
    channels.map((channel) => channelName(channel, channels)),
    [`shared (w11 · ${keyA})`, `shared (w22 · ${keyB})`],
  );
  assert.equal(channelName(parsed(snapshot()), channels), "alpha");
});
test("channel ordering uses display name, then workspace id, then key", () => {
  const state = apply(
    apply(initialState(), snapshot(keyB, "zeta", "w1")),
    snapshot(keyA, "alpha", "w2"),
  );
  assert.deepEqual(
    sortedChannels(state).map((channel) => channel.displayName),
    ["alpha", "zeta"],
  );
});
test("unknown wire versions leave state unchanged", () => {
  const state = apply(initialState(), snapshot());
  const unknown = snapshot(keyB, "new channel", "w2");
  unknown.frame.wire = 2;
  assert.equal(parseEvent("space", JSON.stringify(unknown)), null);
  const next = apply(state, unknown);
  assert.equal(next, state);
  assert.equal(next.channels.has(parsed(snapshot(keyB)).key), false);
});
test("malformed JSON, names, keys, or mismatched envelopes never install partial state", () => {
  assert.equal(parseEvent("space", "{"), null);
  assert.equal(parseEvent("history", "{}"), null);
  for (const key of ["", "a".repeat(19), "A".repeat(20), "g".repeat(20)]) {
    assert.equal(parseEvent("gone", JSON.stringify({ key })), null);
  }
  assert.equal(
    parseEvent("space", JSON.stringify({ ...snapshot(), key: keyB })),
    null,
  );
  assert.equal(parseEvent("space", "null"), null);
  assert.equal(parseEvent("space", "[]"), null);
});
test("parser validates every retained participant, health, attention, and identity field", () => {
  const base = snapshot();
  const invalid = [
    { ...base.frame, participants: [participant(), participant()] },
    { ...base.frame, participants: [{ ...participant(), address_id: "" }] },
    { ...base.frame, participants: [{ ...participant(), activity: "asleep" }] },
    {
      ...base.frame,
      participants: [{ ...participant(), kind: { type: "robot" } }],
    },
    {
      ...base.frame,
      participants: [{ ...participant(), kind: { type: "agent", runtime: 5 } }],
    },
    {
      ...base.frame,
      participants: [
        { ...participant(), role: { text: "role", truncated: "yes" } },
      ],
    },
    {
      ...base.frame,
      participants: [
        { ...participant(), pane_label: { text: 8, truncated: false } },
      ],
    },
    { ...base.frame, participants: [{ ...participant(), alias: 5 }] },
    { ...base.frame, messages: [{ message_id: "" }] },
    { ...base.frame, messages: [{ message_id: "m" }, { message_id: "m" }] },
    { ...base.frame, messages: {} },
    { ...base.frame, health: { level: "busy", in_flight: -1, working: 0 } },
    { ...base.frame, health: { level: "busy", in_flight: 1.5, working: 0 } },
    { ...base.frame, health: { level: "needs_you", items: "2" } },
    { ...base.frame, health: { level: "unknown" } },
    { ...base.frame, space: { ...base.frame.space, display_name: "" } },
    { ...base.frame, space: { ...base.frame.space, workspace_id: "" } },
    { ...base.frame, space: { ...base.frame.space, addressable_label: 8 } },
    {
      ...base.frame,
      attention: [
        {
          source_kind: "other",
          summary: { text: "help", truncated: false },
          opened_at_ms: 1,
        },
      ],
    },
    {
      ...base.frame,
      attention: [
        {
          source_kind: "coordination",
          summary: { text: "help", truncated: false },
          opened_at_ms: -1,
        },
      ],
    },
  ];
  for (const frame of invalid)
    assert.equal(
      parseEvent("space", JSON.stringify({ key: keyA, frame })),
      null,
      JSON.stringify(frame),
    );
});
test("parser accepts opaque human IDs, all activities, safe text, null addressability, and additive fields", () => {
  const value = snapshot();
  const activities: Activity[] = [
    "retired",
    "offline",
    "blocked",
    "working",
    "waiting",
    "idle",
  ];
  const channel = parsed({
    key: keyA,
    frame: {
      ...value.frame,
      space: { ...value.frame.space, addressable_label: null },
      participants: [
        { ...participant("human"), kind: { type: "human" } },
        ...activities.map((activity) => ({
          ...participant(activity, activity),
          kind: { type: "terminal" },
          role: { text: "<script>alert(1)</script>", truncated: true },
        })),
      ],
      extra: true,
    },
  });
  assert.equal(channel.addressableLabel, null);
  assert.equal(channel.members.size, 7);
  assert.equal(
    [...channel.members.values()][1]?.role?.text,
    "<script>alert(1)</script>",
  );
  assert.equal(parsed(snapshot()).addressableLabel, "alpha");
});
test("new channels establish a baseline while background message, member, and activity changes mark unread", () => {
  const state = twoChannels();
  const beta = parsed(snapshot(keyB, "beta", "w2"));
  assert.equal(state.unread.size, 0);
  assert.ok(apply(state, changedChannel()).unread.has(beta.key));
  const messages = snapshot(keyB, "beta", "w2");
  messages.frame.messages.push({ message_id: "m2" });
  assert.ok(apply(state, messages).unread.has(beta.key));
  const members = snapshot(keyB, "beta", "w2");
  members.frame.participants.push(participant("agent-2"));
  assert.ok(apply(state, members).unread.has(beta.key));
  members.frame.participants = [];
  assert.ok(apply(state, members).unread.has(beta.key));
});
test("attention content changes mark unread independently of health", () => {
  const state = twoChannels();
  const beta = snapshot(keyB, "beta", "w2");
  const value = {
    key: keyB,
    frame: {
      ...beta.frame,
      attention: [
        {
          source_kind: "coordination",
          summary: { text: "Approve this", truncated: false },
          opened_at_ms: 1,
        },
      ],
    },
  };
  const next = apply(state, value);
  assert.equal(
    [...next.channels.values()].filter(
      (channel) => channel.attention.length > 0,
    ).length,
    1,
  );
  assert.ok(next.unread.has(parsed(beta).key));
  assert.equal(apply(next, value), next);
});
test("name, health, alias, role, and member ordering changes do not create unread activity", () => {
  const state = twoChannels();
  const beta = snapshot(keyB, "renamed", "w2");
  const value = {
    key: keyB,
    frame: {
      ...beta.frame,
      health: { level: "busy", in_flight: 2, working: 0 },
      participants: [
        {
          ...participant(),
          alias: "new",
          role: { text: "Builder", truncated: false },
        },
      ],
    },
  };
  const next = apply(state, value);
  assert.notEqual(next, state);
  assert.equal(next.unread.size, 0);
  const first = snapshot();
  first.frame.participants.push(participant("agent-2"));
  const before = apply(initialState(), first);
  assert.equal(
    apply(before, {
      ...first,
      frame: {
        ...first.frame,
        participants: [...first.frame.participants].reverse(),
      },
    }).unread.size,
    0,
  );
});
test("visible selected activity stays read, while hidden activity clears only after visibility returns", () => {
  const beta = parsed(snapshot(keyB, "beta", "w2"));
  const state = reduce(twoChannels(), { type: "select", key: beta.key });
  assert.equal(apply(state, changedChannel()).unread.size, 0);
  const hidden = reduce(state, { type: "visibility", visible: false });
  const unread = apply(hidden, changedChannel());
  assert.ok(unread.unread.has(beta.key));
  assert.ok(
    reduce(unread, { type: "select", key: beta.key }).unread.has(beta.key),
  );
  assert.equal(
    reduce(unread, { type: "visibility", visible: true }).unread.size,
    0,
  );
});
test("select clears background unread only for an existing channel", () => {
  const state = apply(twoChannels(), changedChannel());
  const key = parsed(snapshot(keyB, "beta", "w2")).key;
  const next = reduce(state, { type: "select", key });
  assert.equal(next.unread.size, 0);
  assert.equal(next.selectedKey, key);
  assert.equal(
    reduce(initialState(), { type: "select", key }).selectedKey,
    null,
  );
  assert.equal(reduce(next, { type: "select", key }), next);
});
function ready(state: State, keys: string[]): State {
  const event = parseEvent("ready", JSON.stringify({ keys }));
  assert.ok(event, "ready keys must parse");
  return reduce(state, event);
}
test("fix2 item 3: reconnect retains gamma selection while alpha sorts first and beta stays unread", () => {
  const alpha = parsed(snapshot()).key;
  const beta = parsed(snapshot(keyB, "beta", "w2")).key;
  const gamma = parsed(snapshot("c".repeat(20), "gamma", "w3"));
  const before = reduce(
    apply(
      apply(twoChannels(), changedChannel()),
      snapshot(gamma.key, "gamma", "w3"),
    ),
    {
      type: "select",
      key: gamma.key,
    },
  );
  const disconnected = reduce(before, { type: "disconnected" });
  const opened = reduce(disconnected, { type: "opened" });
  assert.equal(opened.channels, before.channels);
  assert.equal(opened.selectedKey, gamma.key);
  assert.equal(opened.unread, before.unread);
  const partial = apply(opened, changedChannel());
  assert.equal(partial.channels, before.channels);
  assert.equal(partial.selectedKey, gamma.key);
  assert.equal(partial.unread, before.unread);
  const completed = ready(
    apply(apply(partial, snapshot()), snapshot(gamma.key, "gamma", "w3")),
    [keyB, keyA, gamma.key],
  );
  assert.equal(completed.selectedKey, gamma.key);
  assert.ok(completed.unread.has(beta));
  assert.equal(completed.channels.get(alpha), before.channels.get(alpha));
});
test("item 2: initial ready selects sidebar order and ready removes omitted keys atomically", () => {
  const opened = reduce(initialState(), { type: "opened" });
  const pending = apply(
    apply(opened, snapshot(keyA, "zeta")),
    snapshot(keyB, "alpha"),
  );
  assert.equal(pending.channels.size, 0);
  const complete = ready(pending, [keyA, keyB]);
  assert.equal(complete.selectedKey, parsed(snapshot(keyB)).key);
  const reconnect = reduce(complete, { type: "opened" });
  const replacement = ready(apply(reconnect, snapshot(keyA, "zeta")), [keyA]);
  assert.equal(replacement.channels.size, 1);
  assert.equal(replacement.selectedKey, parsed(snapshot()).key);
  const empty = ready(reduce(replacement, { type: "opened" }), []);
  assert.equal(empty.channels.size, 0);
  assert.equal(empty.selectedKey, null);
  assert.equal(empty.unread.size, 0);
});
test("interrupted replay and duplicate ready cannot replace the view", () => {
  const before = twoChannels();
  const pending = apply(reduce(before, { type: "opened" }), changedChannel());
  const disconnected = reduce(pending, { type: "disconnected" });
  assert.equal(disconnected.channels, before.channels);
  assert.equal(ready(disconnected, [keyB]), disconnected);
  const complete = ready(
    apply(reduce(disconnected, { type: "opened" }), snapshot()),
    [keyA],
  );
  assert.equal(ready(complete, []), complete);
});
test("item 2: changed replay activity marks only unobserved surviving channels unread", () => {
  const before = twoChannels();
  const replay = apply(
    apply(reduce(before, { type: "opened" }), snapshot()),
    changedChannel(),
  );
  assert.ok(ready(replay, [keyA, keyB]).unread.has(parsed(snapshot(keyB)).key));
  const hidden = reduce(before, { type: "visibility", visible: false });
  const alpha = snapshot();
  alpha.frame.participants = [participant("agent-1", "working")];
  const next = ready(
    apply(apply(reduce(hidden, { type: "opened" }), alpha), snapshot(keyB)),
    [keyA, keyB],
  );
  assert.ok(next.unread.has(parsed(alpha).key));
});
test("item 2: ready rejects malformed and missing keys", () => {
  for (const value of [
    {},
    { keys: null },
    { keys: ["no"] },
    { keys: [keyA, 1] },
  ])
    assert.equal(parseEvent("ready", JSON.stringify(value)), null);
});

test("messages leaving the live window replace retained IDs without marking background unread", () => {
  const state = twoChannels();
  const beta = snapshot(keyB, "beta", "w2");
  beta.frame.messages = [];
  const next = apply(state, beta);
  const key = parsed(beta).key;
  assert.notEqual(next, state);
  assert.equal(next.unread.has(key), false);
  assert.equal(next.channels.get(key)?.messages.size, 0);
  assert.equal(state.channels.get(key)?.messages.size, 1);
});

test("repeated workspace IDs have unique full-key suffixes", () => {
  const keys = [
    "abcdef00000000000000",
    "abcdef00000000000001",
    "abcdee00000000000000",
  ];
  const state = keys.reduce(
    (state, key) => apply(state, snapshot(key, "shared", "w1")),
    initialState(),
  );
  const channels = sortedChannels(state);
  const names = channels.map((channel) => channelName(channel, channels));
  assert.equal(new Set(names).size, 3);
  for (const [index, name] of names.entries()) {
    assert.ok(name.startsWith("shared (w1 · "));
    assert.ok(name.endsWith(")"));
    const suffix = name.slice("shared (w1 · ".length, -1);
    assert.equal(suffix.length, 20);
    assert.ok(channels[index]?.key.startsWith(suffix));
  }
  assert.deepEqual(
    [...channels]
      .reverse()
      .map((channel) => channelName(channel, channels))
      .reverse(),
    names,
  );
});
test("item 7: overflow fallback timestamps preserve the channel while counts stay safe", () => {
  const value = snapshot();
  const overflow = Number("9223372036854775807");
  const frame = {
    ...value.frame,
    attention: [
      {
        source_kind: "coordination",
        summary: { text: "Help", truncated: false },
        opened_at_ms: overflow,
      },
    ],
  };
  assert.equal(parsed({ ...value, frame }).attention[0]?.openedAt, overflow);
  for (const opened_at_ms of [-1, 1.5, Number.POSITIVE_INFINITY])
    assert.equal(
      parseEvent(
        "space",
        JSON.stringify({
          ...value,
          frame: {
            ...frame,
            attention: [{ ...frame.attention[0], opened_at_ms }],
          },
        }),
      ),
      null,
    );
  assert.equal(
    parseEvent(
      "space",
      JSON.stringify({
        ...value,
        frame: {
          ...frame,
          health: { level: "needs_you", items: overflow },
        },
      }),
    ),
    null,
  );
});

test("fix2 item 1: rejected replay frame keeps listed last-good data and other channels resume live", () => {
  const before = twoChannels();
  const bad = snapshot();
  bad.frame.wire = 99;
  const pending = apply(
    apply(reduce(before, { type: "opened" }), bad),
    changedChannel(),
  );
  const next = ready(pending, [keyA, keyB, "d".repeat(20)]);
  assert.equal(next.sync.kind, "live");
  assert.equal(
    next.channels.get(parsed(snapshot()).key),
    before.channels.get(parsed(snapshot()).key),
  );
  assert.equal(
    next.channels.size,
    2,
    "unparsed new keys cannot invent channels",
  );
  const live = apply(next, snapshot(keyB, "renamed live", "w2"));
  assert.equal(
    live.channels.get(parsed(snapshot(keyB)).key)?.displayName,
    "renamed live",
  );
});
for (const [name, keys, expected] of [
  ["subset", [keyB], [keyB]],
  ["superset", [keyA, keyB, "d".repeat(20)], [keyA, keyB]],
  ["duplicates", [keyA, keyA, keyB], [keyA, keyB]],
] satisfies [string, string[], string[]][]) {
  test(`fix2 item 2: ready ${name} ends replay and prunes only omitted keys`, () => {
    const replay = apply(
      apply(reduce(twoChannels(), { type: "opened" }), snapshot()),
      changedChannel(),
    );
    const next = ready(replay, keys);
    assert.equal(next.sync.kind, "live");
    assert.deepEqual([...next.channels.keys()].sort(), expected);
  });
}
test("fix3 item 1: timeout publishes first load and reconnect without pruning and keeps updating", () => {
  const opened = reduce(initialState(), { type: "opened" });
  const first = reduce(apply(opened, snapshot(keyB, "beta", "w2")), {
    type: "replay-timeout",
  });
  assert.equal(first.sync.kind, "fallback");
  assert.equal(first.selectedKey, parsed(snapshot(keyB)).key);
  const before = apply(first, snapshot());
  const next = reduce(
    apply(reduce(before, { type: "opened" }), changedChannel()),
    { type: "replay-timeout" },
  );
  assert.equal(next.sync.kind, "fallback");
  assert.equal(next.channels.size, 2);
  assert.equal(next.selectedKey, before.selectedKey);
  assert.equal(reduce(next, { type: "replay-timeout" }), next);
  assert.equal(
    apply(next, snapshot(keyB, "live beta", "w2")).channels.get(
      parsed(snapshot(keyB)).key,
    )?.displayName,
    "live beta",
  );
});
test("fix2 item 4: visible fallback selection clears its existing unread mark", () => {
  const before = apply(twoChannels(), changedChannel());
  const beta = parsed(snapshot(keyB)).key;
  assert.ok(before.unread.has(beta));
  const next = ready(
    apply(reduce(before, { type: "opened" }), changedChannel()),
    [keyB],
  );
  assert.equal(next.selectedKey, beta);
  assert.equal(next.unread.has(beta), false);
  const hidden = reduce(before, { type: "visibility", visible: false });
  assert.equal(
    ready(apply(reduce(hidden, { type: "opened" }), changedChannel()), [
      keyB,
    ]).unread.has(beta),
    true,
  );
});
test("fix2 item 5: duplicate display suffix stays fixed across new peers with different or identical workspace IDs", () => {
  const first = parsed(snapshot("abcdef11111111111111", "shared", "w1"));
  const second = parsed(snapshot("abcdef22222222222222", "shared", "w2"));
  const third = parsed(snapshot("abcdef11111111111112", "shared", "w1"));
  const label = channelName(first, [first, second]);
  assert.equal(label, `shared (w1 · ${first.key})`);
  assert.equal(channelName(first, [first, second, third]), label);
  assert.equal(
    channelName(third, [first, second, third]),
    `shared (w1 · ${third.key})`,
  );
});
test("fix2 item 6: gone removes committed and pending snapshots during replay and later space may recreate", () => {
  const alpha = parsed(snapshot()).key;
  const before = twoChannels();
  const replay = apply(reduce(before, { type: "opened" }), snapshot());
  const gone = reduce(replay, { type: "gone", key: alpha });
  assert.equal(gone.channels.has(alpha), false);
  assert.equal(gone.selectedKey, null);
  assert.equal(gone.sync.kind, "replaying");
  assert.ok(gone.sync.kind === "replaying" && !gone.sync.pending.has(alpha));
  assert.equal(ready(gone, [keyA, keyB]).channels.has(alpha), false);
  assert.equal(
    ready(apply(gone, snapshot()), [keyA, keyB]).channels.has(alpha),
    true,
  );
  const pendingOnly = apply(
    reduce(initialState(), { type: "opened" }),
    snapshot(),
  );
  const removed = reduce(pendingOnly, { type: "gone", key: alpha });
  assert.equal(ready(removed, [keyA]).channels.size, 0);
});

test("fix3 item 1: late ready prunes the current map and retains gamma selection and alpha unread", () => {
  const gamma = snapshot("c".repeat(20), "gamma", "w3");
  const gammaKey = parsed(gamma).key;
  const alphaKey = parsed(snapshot()).key;
  const before = reduce(apply(twoChannels(), gamma), {
    type: "select",
    key: gammaKey,
  });
  const fallback = reduce(reduce(before, { type: "opened" }), {
    type: "replay-timeout",
  });
  const alpha = snapshot();
  alpha.frame.messages.push({ message_id: "new-after-timeout" });
  const current = apply(
    apply(fallback, alpha),
    snapshot(gammaKey, "gamma-current", "w3"),
  );
  assert.ok(current.unread.has(alphaKey));
  const bad = snapshot();
  bad.frame.wire = 99;
  const next = ready(apply(current, bad), [keyA, gammaKey, "d".repeat(20)]);
  assert.deepEqual([...next.channels.keys()].sort(), [keyA, gammaKey]);
  assert.equal(next.channels.get(alphaKey), current.channels.get(alphaKey));
  assert.equal(next.channels.get(gammaKey)?.displayName, "gamma-current");
  assert.equal(next.selectedKey, gammaKey);
  assert.deepEqual([...next.unread], [alphaKey]);
  assert.equal(next.sync.kind, "live");
});

test("fix3 item 1: late ready removes selected gamma and selects the sorted visible fallback as read", () => {
  const gamma = parsed(snapshot("c".repeat(20), "gamma", "w3"));
  const beta = parsed(snapshot(keyB, "beta", "w2"));
  const before = reduce(
    apply(twoChannels(), snapshot(gamma.key, "gamma", "w3")),
    { type: "select", key: gamma.key },
  );
  const fallback = reduce(reduce(before, { type: "opened" }), {
    type: "replay-timeout",
  });
  const alpha = snapshot(keyA, "zeta");
  const current = apply(apply(fallback, alpha), changedChannel());
  assert.ok(current.unread.has(beta.key));
  const next = ready(current, [keyA, keyB]);
  assert.equal(next.channels.has(gamma.key), false);
  assert.equal(next.selectedKey, beta.key);
  assert.equal(next.unread.has(beta.key), false);
  const hidden = reduce(current, { type: "visibility", visible: false });
  assert.ok(ready(hidden, [keyA, keyB]).unread.has(beta.key));
});

test("fix3 item 1: each open accepts only its first ready and disconnected markers cannot prune", () => {
  const fallback = reduce(reduce(twoChannels(), { type: "opened" }), {
    type: "replay-timeout",
  });
  const first = ready(fallback, [keyB]);
  assert.deepEqual([...first.channels.keys()], [keyB]);
  const later = apply(first, snapshot());
  assert.equal(ready(later, [keyB]), later);
  assert.equal(reduce(later, { type: "replay-timeout" }), later);
  const disconnected = reduce(reduce(later, { type: "opened" }), {
    type: "disconnected",
  });
  assert.equal(ready(disconnected, []), disconnected);
  assert.equal(reduce(disconnected, { type: "replay-timeout" }), disconnected);
  const reopened = ready(reduce(disconnected, { type: "opened" }), []);
  assert.equal(reopened.channels.size, 0);
  assert.equal(reopened.sync.kind, "live");
});

test("fix3 item 2: empty timeout and interrupted staged snapshots stay unknown across retries", () => {
  const initial = initialState();
  assert.equal(initial.viewKnown, false);
  const staged = apply(reduce(initial, { type: "opened" }), snapshot());
  assert.equal(staged.viewKnown, false);
  const disconnected = reduce(staged, { type: "disconnected" });
  assert.equal(disconnected.channels.size, 0);
  assert.equal(disconnected.viewKnown, false);
  const empty = reduce(reduce(disconnected, { type: "opened" }), {
    type: "replay-timeout",
  });
  assert.equal(empty.channels.size, 0);
  assert.equal(empty.viewKnown, false);
  assert.equal(connectionStatus(empty), "connecting");
  assert.equal(reduce(empty, { type: "replay-timeout" }), empty);
  const retry = reduce(reduce(empty, { type: "disconnected" }), {
    type: "opened",
  });
  assert.equal(connectionStatus(retry), "connecting");
  assert.equal(retry.viewKnown, false);
});

test("fix3 item 2: committed data and authoritative empty ready establish lasting view knowledge", () => {
  const opened = reduce(initialState(), { type: "opened" });
  const fallback = reduce(opened, { type: "replay-timeout" });
  const data = apply(fallback, snapshot());
  assert.equal(data.viewKnown, true);
  assert.equal(connectionStatus(data), "live");
  const gone = reduce(data, { type: "gone", key: parsed(snapshot()).key });
  assert.equal(gone.channels.size, 0);
  assert.equal(gone.viewKnown, true);
  assert.equal(connectionStatus(gone), "live");
  const committedReplay = reduce(apply(opened, snapshot()), {
    type: "replay-timeout",
  });
  assert.equal(committedReplay.viewKnown, true);
  const empty = ready(fallback, []);
  assert.equal(empty.viewKnown, true);
  assert.equal(connectionStatus(empty), "live");
  const retry = reduce(reduce(empty, { type: "disconnected" }), {
    type: "opened",
  });
  assert.equal(retry.viewKnown, true);
  assert.equal(connectionStatus(retry), "reconnecting");
  assert.equal(
    connectionStatus(reduce(retry, { type: "replay-timeout" })),
    "live",
  );
});

for (const beforeTimeout of [false, true]) {
  test(`fix3 item 1: late initial ready selects sidebar order after ${beforeTimeout ? "trickling" : "empty"} fallback unless the user chose`, () => {
    const opened = reduce(initialState(), { type: "opened" });
    const beta = snapshot(keyB, "beta", "w2");
    const pending = beforeTimeout ? apply(opened, beta) : opened;
    const fallback = reduce(pending, { type: "replay-timeout" });
    const current = apply(apply(fallback, beta), snapshot());
    const betaKey = parsed(beta).key;
    assert.equal(current.selectedKey, betaKey);
    assert.equal(
      ready(current, [keyA, keyB]).selectedKey,
      parsed(snapshot()).key,
    );
    const chosen = reduce(current, { type: "select", key: betaKey });
    assert.equal(ready(chosen, [keyA, keyB]).selectedKey, betaKey);
  });
}
