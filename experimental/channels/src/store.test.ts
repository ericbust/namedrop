import assert from "node:assert/strict";
import { test } from "node:test";
import type { Activity, Channel, State } from "./model.ts";
import { channelName, initialState, reduce, sortedChannels } from "./store.ts";
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
        addressable_label: `#${name}`,
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
    ["shared (w11)", "shared (w22)"],
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
  assert.equal(
    apply(state, { ...snapshot(), frame: { ...snapshot().frame, wire: 2 } }),
    state,
  );
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
  assert.equal(parsed(snapshot()).addressableLabel, "#alpha");
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
test("attention content changes mark unread independently of health and expose Needs you", () => {
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
test("disconnect retains last good state; open resets replay baseline and reconciles missed gone", () => {
  const before = apply(twoChannels(), changedChannel());
  const disconnected = reduce(before, { type: "disconnected" });
  assert.equal(disconnected.channels, before.channels);
  assert.equal(disconnected.selectedKey, before.selectedKey);
  assert.equal(disconnected.connection, "reconnecting");
  assert.equal(reduce(disconnected, { type: "disconnected" }), disconnected);
  const opened = reduce(disconnected, { type: "opened" });
  assert.equal(opened.channels.size, 0);
  assert.equal(opened.unread.size, 0);
  assert.equal(opened.selectedKey, null);
  assert.equal(opened.connection, "live");
  const replay = apply(opened, snapshot());
  assert.equal(replay.channels.size, 1);
  assert.equal(replay.unread.size, 0);
  assert.equal(
    replay.channels.has(parsed(snapshot(keyB, "beta", "w2")).key),
    false,
  );
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
