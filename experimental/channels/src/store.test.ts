import assert from "node:assert/strict";
import { test } from "node:test";
import type { Activity, ChannelSnapshot, State } from "./model.ts";
import {
  channelSuffix,
  connectionStatus,
  exitFor,
  homesOf,
  initialState,
  type MessageRow,
  reduce,
  replyRecipients,
  participant as resolve,
  retryId,
  runsByMessage,
  sortedChannels,
  type TimelineRow,
  threadGroups,
  timelineView,
} from "./store.ts";
import { parseEvent, parseHistory } from "./wire.ts";

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
function message(message_id = "m1"): {
  message_id: string;
  thread_id: string;
  in_reply_to: string | null;
  at_ms: number;
  author: string;
  kind: string;
  body: { text: string; truncated: boolean };
  legs: { to: string; stage: { stage: string } }[];
} {
  return {
    message_id,
    thread_id: message_id,
    in_reply_to: null,
    at_ms: 100,
    author: "agent-1",
    kind: "mention",
    body: { text: message_id, truncated: false },
    legs: [],
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
      messages: [message()],
      threads: [],
      attention: [],
    },
  };
}
function parsed(value: unknown): ChannelSnapshot {
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
  assert.deepEqual(after.channels.get(channel.key)?.messages, channel.messages);
  assert.equal(after.selection.key, channel.key);
  assert.equal(after.unread.size, 0);
  assert.equal(before.channels.size, 0);
});
test("semantic replay is an identity no-op despite timestamps and unused fields", () => {
  const value = snapshot();
  value.frame.messages.push(message("m2"));
  const state = apply(initialState(), value);
  const replay = {
    ...value,
    ignored: true,
    frame: {
      ...value.frame,
      generated_at_ms: 900,
      messages: [...value.frame.messages],
    },
  };
  assert.equal(apply(state, replay), state);
});
test("snapshots replace members while preserving accumulated messages", () => {
  const state = apply(initialState(), snapshot());
  const value = snapshot();
  value.frame.messages = [];
  value.frame.participants = [];
  const next = apply(state, value);
  assert.equal([...next.channels.values()][0]?.messages.size, 1);
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
    reduce(selected, { type: "gone", key: channel.key }).selection.key,
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
    channels.map((channel) => channelSuffix(channel, channels)),
    [`w11 · ${keyA}`, `w22 · ${keyB}`],
  );
  assert.equal(channelSuffix(parsed(snapshot()), channels), null);
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
test("fix5: channel collation preserves default locale Unicode ordering and every tie breaker", () => {
  const values = [
    snapshot("c".repeat(20), "éclair", "w1"),
    snapshot("d".repeat(20), "eclair", "w1"),
    snapshot("e".repeat(20), "Alpha", "w1"),
    snapshot("f".repeat(20), "alpha", "w1"),
    snapshot("1".repeat(20), "shared", "w2"),
    snapshot("2".repeat(20), "shared", "w10"),
    snapshot("3".repeat(20), "shared", "w10"),
  ];
  const expected = [...values]
    .sort(
      (a, b) =>
        a.frame.space.display_name.localeCompare(b.frame.space.display_name) ||
        a.frame.space.workspace_id.localeCompare(b.frame.space.workspace_id) ||
        a.key.localeCompare(b.key),
    )
    .map((value) => value.key);
  const replay = [...values]
    .reverse()
    .reduce(
      (state, value) => apply(state, value),
      reduce(initialState(), { type: "opened" }),
    );
  const state = ready(
    replay,
    values.map((value) => value.key),
  );
  assert.deepEqual(
    sortedChannels(state).map((channel) => channel.key),
    expected,
  );
  assert.equal(state.selection.key, expected[0]);
  assert.deepEqual(
    sortedChannels(state)
      .filter((channel) => channel.displayName === "shared")
      .map((channel) => channel.key),
    ["2".repeat(20), "3".repeat(20), "1".repeat(20)],
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
for (const source of ["agent_status", "future_attention"]) {
  test(`fix5: ${source} attention parses and replaces a live channel`, () => {
    const base = snapshot();
    const before = apply(initialState(), base);
    const next = apply(before, {
      ...base,
      frame: {
        ...base.frame,
        health: { level: "needs_you", items: 1 },
        participants: [participant("agent-1", "blocked")],
        attention: [
          {
            source_kind: source,
            summary: { text: "Waiting for approval", truncated: true },
            opened_at_ms: 42,
          },
        ],
      },
    });
    assert.notEqual(next, before, "the new snapshot must replace the channel");
    const channel = [...next.channels.values()][0];
    assert.equal(channel?.members.values().next().value?.activity, "blocked");
    assert.deepEqual(channel?.health, { level: "needs_you", items: 1 });
    assert.deepEqual(
      channel?.attention.map(({ source, summary, openedAt }) => ({
        source,
        summary,
        openedAt,
      })),
      [
        {
          source: source === "agent_status" ? "agent_status" : "unknown",
          summary: { text: "Waiting for approval", truncated: true },
          openedAt: 42,
        },
      ],
    );
    assert.equal(channel?.messages.size, 1);
  });
}
test("fix5: attention row keys distinguish duplicate content and stay stable across distinct insertion and reorder", () => {
  const base = snapshot();
  function row(source_kind: string, text = "Same request", truncated = false) {
    return { source_kind, summary: { text, truncated }, opened_at_ms: 42 };
  }
  function rows(attention: ReturnType<typeof row>[]) {
    return parsed({ ...base, frame: { ...base.frame, attention } }).attention;
  }
  const first = row("future_one");
  const second = row("future_two");
  const other = row("coordination", "Other request");
  const original = rows([first, second, first, other]);
  const keys = original.map((item) => item.rowKey);
  for (const key of keys) assert.equal(typeof key, "string");
  assert.equal(
    new Set(keys).size,
    4,
    "distinct future kinds and exact duplicates need distinct row keys",
  );
  assert.deepEqual(
    original.slice(0, 3).map((item) => item.source),
    ["unknown", "unknown", "unknown"],
  );
  assert.deepEqual(
    rows([first, second, first, other]).map((item) => item.rowKey),
    keys,
  );
  const changed = rows([
    other,
    row("future_inserted", "Inserted request"),
    first,
    second,
    first,
  ]);
  assert.deepEqual(
    [
      changed[2]?.rowKey,
      changed[3]?.rowKey,
      changed[4]?.rowKey,
      changed[0]?.rowKey,
    ],
    keys,
  );
  assert.notEqual(
    rows([row("future_one", "Same request", true)])[0]?.rowKey,
    keys[0],
    "truncation changes the row identity",
  );
});
for (const field of ["activity", "health", "member kind"]) {
  test(`fix5: future ${field} keeps the channel and normalizes at the boundary`, () => {
    const base = snapshot();
    const frame = {
      ...base.frame,
      health:
        field === "health" ? { level: "future_health" } : base.frame.health,
      participants: [
        {
          ...participant(),
          activity: field === "activity" ? "future_activity" : "idle",
          kind:
            field === "member kind"
              ? { type: "future_kind" }
              : participant().kind,
        },
      ],
    };
    const next = apply(apply(initialState(), base), { ...base, frame });
    const channel = [...next.channels.values()][0];
    assert.ok(channel);
    const member = [...channel.members.values()][0];
    if (field === "activity") assert.equal(member?.activity, "unknown");
    if (field === "health")
      assert.deepEqual(channel.health, { level: "unknown" });
    if (field === "member kind")
      assert.deepEqual(member?.kind, { type: "unknown" });
    assert.equal(channel.messages.size, 1);
  });
}
test("fix5: malformed discriminators and shared or known payloads still reject the whole frame", () => {
  const base = snapshot();
  const attention = {
    source_kind: "future_attention",
    summary: { text: "Help", truncated: false },
    opened_at_ms: 1,
  };
  for (const value of [undefined, null, 7, true, [], {}, "", " \t\n"]) {
    for (const frame of [
      { ...base.frame, health: { level: value } },
      { ...base.frame, participants: [{ ...participant(), activity: value }] },
      {
        ...base.frame,
        participants: [{ ...participant(), kind: { type: value } }],
      },
      { ...base.frame, attention: [{ ...attention, source_kind: value }] },
    ])
      assert.equal(
        parseEvent("space", JSON.stringify({ ...base, frame })),
        null,
      );
  }
  for (const frame of [
    { ...base.frame, health: { level: "busy", working: 1 } },
    { ...base.frame, health: { level: "needs_you" } },
    {
      ...base.frame,
      participants: [{ ...participant(), kind: { type: "agent" } }],
    },
    {
      ...base.frame,
      attention: [{ ...attention, summary: { text: "Help", truncated: 1 } }],
    },
    {
      ...base.frame,
      attention: [{ ...attention, summary: { text: 5, truncated: false } }],
    },
    { ...base.frame, attention: [{ ...attention, opened_at_ms: -1 }] },
    { ...base.frame, attention: [{ ...attention, opened_at_ms: 1.5 }] },
    { ...base.frame, wire: 2, attention: [attention] },
  ])
    assert.equal(parseEvent("space", JSON.stringify({ ...base, frame })), null);
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
    { ...base.frame, participants: [{ ...participant(), activity: 5 }] },
    {
      ...base.frame,
      participants: [{ ...participant(), kind: { type: "" } }],
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
    { ...base.frame, health: { level: null } },
    { ...base.frame, space: { ...base.frame.space, display_name: "" } },
    { ...base.frame, space: { ...base.frame.space, workspace_id: "" } },
    { ...base.frame, space: { ...base.frame.space, addressable_label: 8 } },
    {
      ...base.frame,
      attention: [
        {
          source_kind: [],
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
  messages.frame.messages.push(message("m2"));
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
test("visible selected activity stays read, while hidden activity stays unread until read after visibility returns", () => {
  const beta = parsed(snapshot(keyB, "beta", "w2"));
  const state = reduce(twoChannels(), { type: "select", key: beta.key });
  assert.equal(apply(state, changedChannel()).unread.size, 0);
  const hidden = reduce(state, { type: "visibility", visible: false });
  const unread = apply(hidden, changedChannel());
  assert.ok(unread.unread.has(beta.key));
  assert.ok(
    reduce(unread, { type: "select", key: beta.key }).unread.has(beta.key),
  );
  const shown = reduce(unread, { type: "visibility", visible: true });
  assert.ok(shown.unread.has(beta.key));
  assert.equal(reduce(shown, { type: "read", key: beta.key }).unread.size, 0);
});
test("design-b: select keeps unread, and read clears only the selected key", () => {
  const state = apply(twoChannels(), changedChannel());
  const key = parsed(snapshot(keyB, "beta", "w2")).key;
  const alpha = parsed(snapshot()).key;
  const next = reduce(state, { type: "select", key });
  assert.equal(next.selection.key, key);
  assert.ok(next.unread.has(key));
  assert.equal(
    reduce(initialState(), { type: "select", key }).selection.key,
    null,
  );
  assert.equal(reduce(next, { type: "select", key }), next);
  const elsewhere = reduce(next, { type: "select", key: alpha });
  assert.equal(reduce(elsewhere, { type: "read", key }), elsewhere);
  assert.ok(reduce(elsewhere, { type: "read", key }).unread.has(key));
  const read = reduce(next, { type: "read", key });
  assert.equal(read.unread.size, 0);
  assert.equal(reduce(read, { type: "read", key }), read);
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
  assert.equal(opened.selection.key, gamma.key);
  assert.equal(opened.unread, before.unread);
  const partial = apply(opened, changedChannel());
  assert.equal(partial.channels, before.channels);
  assert.equal(partial.selection.key, gamma.key);
  assert.equal(partial.unread, before.unread);
  const completed = ready(
    apply(apply(partial, snapshot()), snapshot(gamma.key, "gamma", "w3")),
    [keyB, keyA, gamma.key],
  );
  assert.equal(completed.selection.key, gamma.key);
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
  assert.equal(complete.selection.key, parsed(snapshot(keyB)).key);
  const reconnect = reduce(complete, { type: "opened" });
  const replacement = ready(apply(reconnect, snapshot(keyA, "zeta")), [keyA]);
  assert.equal(replacement.channels.size, 1);
  assert.equal(replacement.selection.key, parsed(snapshot()).key);
  const empty = ready(reduce(replacement, { type: "opened" }), []);
  assert.equal(empty.channels.size, 0);
  assert.equal(empty.selection.key, null);
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

test("messages leaving the live window stay loaded without marking background unread", () => {
  const state = twoChannels();
  const beta = snapshot(keyB, "beta", "w2");
  beta.frame.messages = [];
  const next = apply(state, beta);
  const key = parsed(beta).key;
  assert.equal(activeChannel(next).messages, activeChannel(state).messages);
  assert.equal(activeChannel(next).runs, activeChannel(state).runs);
  assert.equal(next.unread, state.unread);
  assert.equal(next.unread.has(key), false);
  assert.equal(next.channels.get(key)?.messages.size, 1);
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
  const names = channels.map((channel) => channelSuffix(channel, channels));
  assert.equal(new Set(names).size, 3);
  for (const [index, name] of names.entries()) {
    assert.ok(name !== null && name.startsWith("w1 · "));
    const suffix = name.slice("w1 · ".length);
    assert.equal(suffix.length, 20);
    assert.ok(channels[index]?.key.startsWith(suffix));
  }
  assert.deepEqual(
    [...channels]
      .reverse()
      .map((channel) => channelSuffix(channel, channels))
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
  assert.equal(first.selection.key, parsed(snapshot(keyB)).key);
  const before = apply(first, snapshot());
  const next = reduce(
    apply(reduce(before, { type: "opened" }), changedChannel()),
    { type: "replay-timeout" },
  );
  assert.equal(next.sync.kind, "fallback");
  assert.equal(next.channels.size, 2);
  assert.equal(next.selection.key, before.selection.key);
  assert.equal(reduce(next, { type: "replay-timeout" }), next);
  assert.equal(
    apply(next, snapshot(keyB, "live beta", "w2")).channels.get(
      parsed(snapshot(keyB)).key,
    )?.displayName,
    "live beta",
  );
});
test("fix2 item 4: fallback selection keeps its unread mark until read on dwell", () => {
  const before = apply(twoChannels(), changedChannel());
  const beta = parsed(snapshot(keyB)).key;
  assert.ok(before.unread.has(beta));
  const next = ready(
    apply(reduce(before, { type: "opened" }), changedChannel()),
    [keyB],
  );
  assert.equal(next.selection.key, beta);
  assert.ok(next.unread.has(beta));
  assert.equal(
    reduce(next, { type: "read", key: beta }).unread.has(beta),
    false,
  );
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
  const label = channelSuffix(first, [first, second]);
  assert.equal(label, `w1 · ${first.key}`);
  assert.equal(channelSuffix(first, [first, second, third]), label);
  assert.equal(
    channelSuffix(third, [first, second, third]),
    `w1 · ${third.key}`,
  );
});
test("fix2 item 6: gone removes committed and pending snapshots during replay and later space may recreate", () => {
  const alpha = parsed(snapshot()).key;
  const before = twoChannels();
  const replay = apply(reduce(before, { type: "opened" }), snapshot());
  const gone = reduce(replay, { type: "gone", key: alpha });
  assert.equal(gone.channels.has(alpha), false);
  assert.equal(gone.selection.key, null);
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
  alpha.frame.messages.push(message("new-after-timeout"));
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
  assert.equal(next.selection.key, gammaKey);
  assert.deepEqual([...next.unread], [alphaKey]);
  assert.equal(next.sync.kind, "live");
});

test("fix3 item 1: late ready removes selected gamma and selects the sorted fallback, unread until read", () => {
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
  assert.equal(next.selection.key, beta.key);
  assert.ok(next.unread.has(beta.key));
  assert.equal(
    reduce(next, { type: "read", key: beta.key }).unread.has(beta.key),
    false,
  );
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
    assert.equal(current.selection.key, parsed(snapshot()).key);
    assert.equal(
      ready(current, [keyA, keyB]).selection.key,
      parsed(snapshot()).key,
    );
    const chosen = reduce(current, { type: "select", key: betaKey });
    assert.equal(ready(chosen, [keyA, keyB]).selection.key, betaKey);
  });
}

function withMessages(ids: string[], key = keyA) {
  const value = snapshot(key);
  value.frame.messages = ids.map((id) => message(id));
  return value;
}
function activeChannel(state: State) {
  const channel =
    state.selection.key && state.channels.get(state.selection.key);
  assert.ok(channel);
  return channel;
}
function requesting(state: State) {
  const next = reduce(state, { type: "older", key: activeChannel(state).key });
  const history = activeChannel(next).history;
  assert.ok(history.kind === "loading");
  return { state: next, request: history.request };
}
function historyPage(state: State, ids: string[]) {
  const pending = requesting(state);
  return reduce(pending.state, {
    type: "history",
    request: pending.request,
    result: {
      kind: "page",
      messages: [...parsed(withMessages(ids)).messages.values()],
    },
  });
}
test("UI-2: history preserves server order for tied timestamps and deduplicates live overlap", () => {
  let state = apply(initialState(), withMessages(["z", "b", "c"]));
  state = historyPage(state, ["x", "a", "z"]);
  assert.deepEqual(activeChannel(state).runs, [["x", "a", "z", "b", "c"]]);
  state = apply(state, withMessages(["b", "c", "d"]));
  assert.deepEqual(activeChannel(state).runs, [["x", "a", "z", "b", "c", "d"]]);
  assert.equal(activeChannel(state).messages.size, 6);
});
test("UI-2: receipt-only live changes apply and delayed history never regresses receipts", () => {
  const value = withMessages(["m1", "m2"]);
  value.frame.messages[0]?.legs.push({
    to: "agent-1",
    stage: { stage: "queued" },
  });
  const pending = requesting(apply(initialState(), value));
  const update = structuredClone(value);
  const leg = update.frame.messages[0]?.legs[0];
  assert.ok(leg);
  leg.stage.stage = "held";
  const live = apply(pending.state, update);
  assert.notEqual(live, pending.state);
  const after = reduce(live, {
    type: "history",
    request: pending.request,
    result: { kind: "page", messages: [...parsed(value).messages.values()] },
  });
  assert.equal(
    activeChannel(after).messages.get("m1")?.legs[0]?.stage.stage,
    "held",
  );
});
test("UI-2: disjoint live windows show a gap and bridge backward until joining older coverage", () => {
  let state = apply(
    apply(initialState(), withMessages(["a", "b"])),
    withMessages(["f", "g"]),
  );
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b"],
    ["f", "g"],
  ]);
  const first = requesting(state);
  assert.equal(first.request.purpose, "bridge");
  assert.equal(first.request.before, "f");
  state = reduce(first.state, {
    type: "history",
    request: first.request,
    result: {
      kind: "page",
      messages: [...parsed(withMessages(["d", "e"])).messages.values()],
    },
  });
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b"],
    ["d", "e", "f", "g"],
  ]);
  state = historyPage(state, ["b", "c"]);
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b", "c", "d", "e", "f", "g"],
  ]);
});
test("UI-2: every replay window survives ready and fallback, including history completed during replay", () => {
  for (const finish of ["ready", "replay-timeout"] as const) {
    const pending = requesting(apply(initialState(), withMessages(["d", "e"])));
    let state = apply(
      apply(
        reduce(pending.state, { type: "opened" }),
        withMessages(["e", "f"]),
      ),
      withMessages(["f", "g"]),
    );
    state = reduce(state, {
      type: "history",
      request: pending.request,
      result: {
        kind: "page",
        messages: [...parsed(withMessages(["a", "b", "c"])).messages.values()],
      },
    });
    state =
      finish === "ready"
        ? ready(state, [keyA])
        : reduce(state, { type: "replay-timeout" });
    assert.deepEqual(activeChannel(state).runs, [
      ["a", "b", "c", "d", "e", "f", "g"],
    ]);
  }
});
test("UI-2: history errors retry, empty older pages exhaust, empty bridges remain explicit", () => {
  const first = requesting(apply(initialState(), withMessages(["a"])));
  const failed = reduce(first.state, {
    type: "history",
    request: first.request,
    result: { kind: "error", reason: "HTTP 404" },
  });
  assert.equal(activeChannel(failed).history.kind, "error");
  const retry = requesting(failed);
  assert.notEqual(retry.request.id, first.request.id);
  const empty = reduce(retry.state, {
    type: "history",
    request: retry.request,
    result: { kind: "page", messages: [] },
  });
  assert.equal(activeChannel(empty).exhausted, true);
  assert.equal(reduce(empty, { type: "older", key: first.request.key }), empty);
  const bridge = requesting(apply(empty, withMessages(["c"])));
  const missing = reduce(bridge.state, {
    type: "history",
    request: bridge.request,
    result: { kind: "page", messages: [] },
  });
  assert.equal(activeChannel(missing).history.kind, "error");
  assert.equal(activeChannel(missing).runs.length, 2);
});
test("UI-2: stale history cannot cross channels or a gone and recreated channel", () => {
  const pending = requesting(twoChannels());
  const beta = parsed(snapshot(keyB)).key;
  const switched = reduce(pending.state, { type: "select", key: beta });
  const completed = reduce(switched, {
    type: "history",
    request: pending.request,
    result: {
      kind: "page",
      messages: [...parsed(withMessages(["older-alpha"])).messages.values()],
    },
  });
  assert.equal(
    completed.channels.get(beta)?.messages.has("older-alpha"),
    false,
  );
  const recreated = apply(
    reduce(pending.state, { type: "gone", key: pending.request.key }),
    withMessages(["new"]),
  );
  const next = requesting(
    reduce(recreated, { type: "select", key: pending.request.key }),
  );
  assert.notEqual(next.request.id, pending.request.id);
  assert.equal(
    reduce(next.state, {
      type: "history",
      request: pending.request,
      result: { kind: "page", messages: [] },
    }),
    next.state,
  );
});
test("UI-2: thread groups derive loaded replies, preserve orphans and reveal fetched parents", () => {
  const value = withMessages(["reply"]);
  const reply = value.frame.messages[0];
  assert.ok(reply);
  reply.thread_id = "thread";
  reply.kind = "reply";
  reply.in_reply_to = "parent";
  let state = apply(initialState(), value);
  const rows = (state: State) =>
    timelineView(activeChannel(state)).filter((row) => row.kind !== "day");
  assert.equal(rows(state)[0]?.kind, "orphan");
  const parent = message("parent");
  parent.thread_id = "thread";
  const pending = requesting(state);
  state = reduce(pending.state, {
    type: "history",
    request: pending.request,
    result: {
      kind: "page",
      messages: [
        ...parsed({
          ...value,
          frame: { ...value.frame, messages: [parent] },
        }).messages.values(),
      ],
    },
  });
  const group = threadGroups(activeChannel(state)).get("thread");
  assert.equal(group?.parents[0]?.id, "parent");
  assert.deepEqual(
    group?.replies.map((row) => row.id),
    ["reply"],
  );
  assert.equal(rows(state).length, 1);
});
test("UI-2: history boundary reverses newest-first once and future kinds and stages remain visible", () => {
  const rows = [message("new"), message("old")];
  rows[0]?.legs.push({ to: "agent-1", stage: { stage: "future-stage" } });
  const row = rows[0];
  assert.ok(row);
  row.kind = "future-kind";
  const parsed = parseHistory(rows);
  assert.deepEqual(
    parsed.map((row) => row.id),
    ["old", "new"],
  );
  assert.equal(parsed[1]?.kind, "unknown");
  assert.equal(parsed[1]?.legs[0]?.stage.stage, "unknown");
  assert.throws(() =>
    parseHistory([{ ...row, body: { text: 42, truncated: false } }]),
  );
});

test("UI-2: replay retains disjoint windows as separate coverage until each bridge is fetched", () => {
  const before = apply(initialState(), withMessages(["a", "b"]));
  const replay = apply(
    apply(reduce(before, { type: "opened" }), withMessages(["f", "g"])),
    withMessages(["k", "l"]),
  );
  let state = ready(replay, [keyA]);
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b"],
    ["f", "g"],
    ["k", "l"],
  ]);
  state = historyPage(state, ["b", "c", "d", "e"]);
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b", "c", "d", "e", "f", "g"],
    ["k", "l"],
  ]);
  state = historyPage(state, ["g", "h", "i", "j"]);
  assert.deepEqual(activeChannel(state).runs, [
    ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"],
  ]);
});
test("UI-2: every real delivery stage retains its payload and invalid known payloads reject the frame", () => {
  const value = withMessages(["receipt"]);
  const stages = [
    { stage: "queued" },
    { stage: "delivered" },
    { stage: "working", acked_at_ms: 12 },
    { stage: "replied", reply_id: "reply" },
    { stage: "completed", kind: "settled" },
    { stage: "held" },
    { stage: "failed" },
    { stage: "cancelled" },
  ];
  const row = value.frame.messages[0];
  assert.ok(row);
  row.legs = stages.map((stage, index) => ({ to: `agent-${index}`, stage }));
  const result = parsed(value).messages.get("receipt");
  assert.deepEqual(
    result?.legs.map((leg) => leg.stage),
    [
      { stage: "queued" },
      { stage: "delivered" },
      { stage: "working", ackedAt: 12 },
      { stage: "replied", replyId: "reply" },
      { stage: "completed", kind: "settled" },
      { stage: "held" },
      { stage: "failed" },
      { stage: "cancelled" },
    ],
  );
  for (const stage of [
    { stage: "working" },
    { stage: "replied" },
    { stage: "completed" },
  ]) {
    row.legs = [{ to: "agent", stage }];
    assert.equal(parseEvent("space", JSON.stringify(value)), null);
  }
});

test("UI-2 fix1: no-op frames retain maps and touched rows reuse unchanged objects", () => {
  const value = withMessages(Array.from({ length: 150 }, (_, n) => `row${n}`));
  const state = apply(initialState(), value);
  const before = activeChannel(state);
  assert.equal(apply(state, structuredClone(value)), state);
  const metadata = structuredClone(value);
  metadata.frame.space.display_name = "Renamed";
  const renamed = activeChannel(apply(state, metadata));
  assert.equal(renamed.messages, before.messages);
  assert.equal(renamed.runs, before.runs);
  assert.equal(renamed.members, before.members);
  const touched = value.frame.messages[70];
  assert.ok(touched);
  touched.legs = [{ to: "agent-1", stage: { stage: "held" } }];
  const after = activeChannel(apply(state, value));
  assert.notEqual(after.messages.get("row70"), before.messages.get("row70"));
  assert.equal(after.runs, before.runs);
  assert.equal(after.members, before.members);
  for (const [id, row] of before.messages)
    if (id !== "row70") assert.equal(after.messages.get(id), row);
});
function numbered(from: number, to: number) {
  return Array.from({ length: to - from + 1 }, (_, n) => `m${n + from}`);
}
function loadedWindow() {
  let state = apply(initialState(), withMessages(numbered(1, 150)));
  for (let last = 250; last <= 750; last += 100)
    state = apply(state, withMessages(numbered(last - 149, last)));
  return state;
}
test("UI-2 fix1: following caps at 600, clears exhaustion and keeps evicted history reachable", () => {
  let state = apply(initialState(), withMessages(numbered(1, 150)));
  state = historyPage(state, []);
  assert.equal(activeChannel(state).exhausted, true);
  for (let last = 250; last <= 750; last += 100)
    state = apply(state, withMessages(numbered(last - 149, last)));
  const channel = activeChannel(state);
  assert.equal(channel.messages.size, 600);
  assert.deepEqual(channel.runs, [numbered(151, 750)]);
  assert.equal(channel.latestMessages.size, 150);
  assert.equal(channel.exhausted, false);
  assert.equal(channel.viewMode, "following");
  const pending = requesting(state);
  assert.equal(pending.request.before, "m151");
  assert.equal(pending.request.purpose, "older");
});
test("UI-2 fix1: capped older pages keep their anchor and walk older while live frames refresh overlap", () => {
  let state = historyPage(loadedWindow(), numbered(101, 150));
  assert.deepEqual(activeChannel(state).runs, [numbered(101, 700)]);
  assert.equal(activeChannel(state).viewMode, "browsing");
  const pending = requesting(state);
  assert.equal(pending.request.before, "m101");
  const live = withMessages(numbered(651, 800));
  const overlap = live.frame.messages.find((row) => row.message_id === "m675");
  assert.ok(overlap);
  overlap.legs = [{ to: "agent-1", stage: { stage: "held" } }];
  state = apply(pending.state, live);
  assert.equal(activeChannel(state).messages.has("m800"), false);
  assert.equal(
    activeChannel(state).messages.get("m675")?.legs[0]?.stage.stage,
    "held",
  );
  assert.equal(activeChannel(state).runs.length, 1);
  state = reduce(state, {
    type: "history",
    request: pending.request,
    result: {
      kind: "page",
      messages: [...parsed(withMessages(numbered(51, 100))).messages.values()],
    },
  });
  assert.deepEqual(activeChannel(state).runs, [numbered(51, 650)]);
  state = historyPage(state, numbered(1, 50));
  state = historyPage(state, []);
  assert.equal(activeChannel(state).exhausted, true);
  const stale = requesting(
    reduce(state, { type: "latest", key: activeChannel(state).key }),
  );
  state = reduce(stale.state, {
    type: "latest",
    key: activeChannel(state).key,
  });
  assert.deepEqual(activeChannel(state).runs, [numbered(651, 800)]);
  assert.equal(activeChannel(state).viewMode, "following");
  assert.equal(activeChannel(state).exhausted, false);
  assert.equal(
    reduce(state, {
      type: "history",
      request: stale.request,
      result: { kind: "page", messages: [] },
    }),
    state,
  );
});
test("UI-2 fix1: replay stays capped without restoring an intentionally evicted newest edge", () => {
  let state = historyPage(loadedWindow(), numbered(101, 150));
  state = apply(
    reduce(state, { type: "opened" }),
    withMessages(numbered(751, 900)),
  );
  assert.equal(state.sync.kind, "replaying");
  if (state.sync.kind === "replaying")
    assert.equal(
      state.sync.pending.get(activeChannel(state).key)?.messages.size,
      600,
    );
  state = ready(state, [keyA]);
  assert.deepEqual(activeChannel(state).runs, [numbered(101, 700)]);
  assert.equal(activeChannel(state).latestMessages.has("m900"), true);
  assert.equal(activeChannel(state).viewMode, "browsing");
  let following = loadedWindow();
  following = apply(
    reduce(following, { type: "opened" }),
    withMessages(numbered(701, 850)),
  );
  following = apply(following, withMessages(numbered(801, 950)));
  following = ready(following, [keyA]);
  assert.deepEqual(activeChannel(following).runs, [numbered(351, 950)]);
  assert.equal(activeChannel(following).messages.size, 600);
});

test("UI-2 fix1: an overlapping frame can join coverage without adding rows", () => {
  let state = apply(initialState(), withMessages(["a", "b"]));
  state = apply(state, withMessages(["c", "d"]));
  assert.equal(activeChannel(state).runs.length, 2);
  state = apply(state, withMessages(["b", "c"]));
  assert.deepEqual(activeChannel(state).runs, [["a", "b", "c", "d"]]);
});
test("UI-2 fix1: oversized snapshots keep only the newest 150 rows as latest source", () => {
  const channel = activeChannel(
    apply(initialState(), withMessages(numbered(1, 900))),
  );
  assert.equal(channel.latestMessages.size, 150);
  assert.deepEqual([...channel.latestMessages.keys()], numbered(751, 900));
  assert.equal(channel.messages.size, 600);
  assert.deepEqual(channel.runs, [numbered(301, 900)]);
  const belowCap = activeChannel(
    apply(initialState(), withMessages(numbered(1, 400))),
  );
  assert.equal(belowCap.messages.size, 400);
  assert.equal(belowCap.latestMessages.size, 150);
});

test("UI-2 fix1: one replay window caps both runs and rows at the same boundary", () => {
  let state = loadedWindow();
  state = apply(
    reduce(state, { type: "opened" }),
    withMessages(numbered(701, 850)),
  );
  state = ready(state, [keyA]);
  const channel = activeChannel(state);
  assert.equal(channel.messages.size, 600);
  assert.deepEqual(channel.runs, [numbered(251, 850)]);
  assert.deepEqual(
    new Set(channel.runs.flat()),
    new Set(channel.messages.keys()),
  );
});

test("UI-2 fix1: evicting a pending history cursor invalidates its stale response", () => {
  const pending = requesting(loadedWindow());
  let state = apply(pending.state, withMessages(numbered(701, 850)));
  assert.equal(activeChannel(state).history.kind, "idle");
  assert.equal(
    reduce(state, {
      type: "history",
      request: pending.request,
      result: {
        kind: "page",
        messages: [
          ...parsed(withMessages(numbered(101, 150))).messages.values(),
        ],
      },
    }),
    state,
  );
  const retry = requesting(state);
  assert.equal(retry.request.before, "m251");
  assert.notEqual(retry.request.id, pending.request.id);
  state = historyPage(state, numbered(201, 250));
  assert.deepEqual(activeChannel(state).runs, [numbered(201, 800)]);
});

test("UI-2 fix1: jumping during replay does not restore discarded browsing history", () => {
  let state = historyPage(loadedWindow(), numbered(101, 150));
  state = apply(
    reduce(state, { type: "opened" }),
    withMessages(numbered(651, 800)),
  );
  state = reduce(state, { type: "latest", key: activeChannel(state).key });
  state = ready(state, [keyA]);
  const channel = activeChannel(state);
  assert.equal(channel.viewMode, "following");
  assert.deepEqual(channel.runs, [numbered(601, 800)]);
  assert.equal(channel.messages.has("m101"), false);
});

test("UI-2 fix1: browsing keeps receipt updates from every staged replay window", () => {
  let state = historyPage(loadedWindow(), numbered(101, 150));
  const before = activeChannel(state);
  state = reduce(state, { type: "opened" });
  const first = withMessages(numbered(651, 800));
  const receipt = first.frame.messages.find((row) => row.message_id === "m675");
  assert.ok(receipt);
  receipt.legs = [{ to: "agent-1", stage: { stage: "held" } }];
  state = apply(state, first);
  state = apply(state, withMessages(numbered(751, 900)));
  state = ready(state, [keyA]);
  const after = activeChannel(state);
  assert.equal(after.messages.get("m675")?.legs[0]?.stage.stage, "held");
  assert.deepEqual(after.runs, before.runs);
  assert.equal(after.messages.size, 600);
  assert.equal(after.messages.has("m900"), false);
});

function fullWindow() {
  let state = apply(initialState(), withMessages(numbered(451, 600)));
  for (let first = 401; first >= 1; first -= 50)
    state = historyPage(state, numbered(first, first + 49));
  assert.deepEqual(activeChannel(state).runs, [numbered(1, 600)]);
  assert.equal(activeChannel(state).viewMode, "following");
  return state;
}
function follow(state: State, atBottom: boolean): State {
  return reduce(state, {
    type: "follow",
    key: activeChannel(state).key,
    atBottom,
  });
}
function withReceipt(value: ReturnType<typeof withMessages>, id: string) {
  const row = value.frame.messages.find((row) => row.message_id === id);
  assert.ok(row);
  row.legs = [{ to: "agent-1", stage: { stage: "held" } }];
  return value;
}
test("history window: a reader scrolled up keeps every loaded row when a live frame passes the cap", () => {
  let state = follow(fullWindow(), false);
  assert.equal(follow(state, false), state);
  state = apply(state, withReceipt(withMessages(numbered(456, 605)), "m456"));
  const channel = activeChannel(state);
  assert.equal(channel.viewMode, "browsing");
  assert.deepEqual(channel.runs, [numbered(1, 600)]);
  assert.deepEqual(new Set(channel.messages.keys()), new Set(numbered(1, 600)));
  assert.equal(channel.messages.get("m456")?.legs[0]?.stage.stage, "held");
  assert.equal(channel.latestMessages.has("m605"), true);
});
test("history window: a reader at the bottom drops the oldest rows and keeps following", () => {
  let state = follow(follow(fullWindow(), false), true);
  state = apply(state, withMessages(numbered(456, 605)));
  const channel = activeChannel(state);
  assert.equal(channel.viewMode, "following");
  assert.deepEqual(channel.runs, [numbered(6, 605)]);
  assert.equal(channel.messages.size, 600);
  assert.equal(channel.messages.has("m5"), false);
});
test("history window: browsing refreshes the rows it shows and stays put until the jump to latest", () => {
  let state = apply(
    follow(fullWindow(), false),
    withMessages(numbered(456, 605)),
  );
  state = follow(state, true);
  state = apply(state, withReceipt(withMessages(numbered(461, 610)), "m500"));
  let channel = activeChannel(state);
  assert.equal(channel.viewMode, "browsing");
  assert.deepEqual(channel.runs, [numbered(1, 600)]);
  assert.equal(channel.messages.get("m500")?.legs[0]?.stage.stage, "held");
  assert.equal(channel.messages.has("m610"), false);
  state = follow(state, false);
  state = reduce(state, { type: "latest", key: channel.key });
  channel = activeChannel(state);
  assert.equal(channel.viewMode, "following");
  assert.equal(channel.atBottom, true);
  assert.deepEqual(channel.runs, [numbered(461, 610)]);
});
test("history window: replay decides from the reader's position when ready arrives", () => {
  let state = follow(fullWindow(), false);
  state = apply(
    reduce(state, { type: "opened" }),
    withReceipt(withMessages(numbered(456, 605)), "m456"),
  );
  let channel = activeChannel(ready(state, [keyA]));
  assert.equal(channel.viewMode, "browsing");
  assert.deepEqual(channel.runs, [numbered(1, 600)]);
  assert.equal(channel.messages.get("m456")?.legs[0]?.stage.stage, "held");
  state = follow(state, true);
  channel = activeChannel(ready(state, [keyA]));
  assert.equal(channel.viewMode, "following");
  assert.deepEqual(channel.runs, [numbered(6, 605)]);
  assert.equal(channel.messages.get("m456")?.legs[0]?.stage.stage, "held");
});

test("design-b: attention subject and message ids parse from the server's shape and the alias gets one @", () => {
  const base = snapshot();
  function frame(extra: Record<string, unknown>) {
    return {
      ...base,
      frame: {
        ...base.frame,
        health: { level: "needs_you", items: 1 },
        attention: [
          {
            source_kind: "delivery_attempt",
            summary: { text: "No reply", truncated: false },
            opened_at_ms: 42,
            ...extra,
          },
        ],
      },
    };
  }
  const plain = apply(
    initialState(),
    frame({ subject_alias: null, message_id: null }),
  );
  const [unnamed] = activeChannel(plain).attention;
  assert.equal(unnamed?.subjectAlias, null);
  assert.equal(unnamed?.messageId, null);
  for (const other of [
    {},
    { subject_alias: 7, message_id: { id: "m1" } },
    { subject_alias: " ", message_id: "" },
    { subject_alias: "@", message_id: " " },
  ]) {
    const [item] = parsed(frame(other)).attention;
    assert.equal(item?.subjectAlias, null);
    assert.equal(item?.messageId, null);
  }
  const withSubject = apply(
    plain,
    frame({ subject_alias: "codex", message_id: "m1" }),
  );
  assert.notEqual(withSubject, plain);
  const [item] = activeChannel(withSubject).attention;
  assert.equal(item?.subjectAlias, "@codex");
  assert.equal(item?.messageId, "m1");
  for (const alias of ["@codex", "@@codex"])
    assert.equal(
      parsed(frame({ subject_alias: alias, message_id: "m1" })).attention[0]
        ?.subjectAlias,
      "@codex",
    );
  assert.equal(
    apply(withSubject, frame({ subject_alias: "codex", message_id: "m1" })),
    withSubject,
  );
});

test("design-b: terminal runs parse tolerantly, keep identity and propagate exit codes", () => {
  const base = snapshot();
  function frame(terminal_runs: unknown) {
    return { ...base, frame: { ...base.frame, terminal_runs } };
  }
  const valid = {
    terminal: "shell-1",
    command: { text: "echo hi", truncated: false },
    started_at_ms: 90,
    exit_code: 0,
    message_id: "m1",
  };
  assert.deepEqual(parsed(base).terminalRuns, []);
  for (const malformed of [null, { runs: [] }, "runs", 4])
    assert.deepEqual(parsed(frame(malformed)).terminalRuns, []);
  assert.deepEqual(
    parsed(
      frame([
        valid,
        { ...valid, terminal: "" },
        { ...valid, command: "echo hi" },
        { ...valid, started_at_ms: -1 },
        "not a run",
        { ...valid, exit_code: null, message_id: null },
        { ...valid, exit_code: null, message_id: undefined },
        { ...valid, exit_code: "1", message_id: 5 },
      ]),
    ).terminalRuns,
    [
      {
        terminal: "shell-1",
        command: { text: "echo hi", truncated: false },
        startedAt: 90,
        exitCode: 0,
        messageId: "m1",
      },
      {
        terminal: "shell-1",
        command: { text: "echo hi", truncated: false },
        startedAt: 90,
        exitCode: null,
        messageId: null,
      },
      {
        terminal: "shell-1",
        command: { text: "echo hi", truncated: false },
        startedAt: 90,
        exitCode: null,
        messageId: null,
      },
      {
        terminal: "shell-1",
        command: { text: "echo hi", truncated: false },
        startedAt: 90,
        exitCode: null,
        messageId: null,
      },
    ],
  );
  const state = apply(initialState(), frame([valid]));
  assert.equal(apply(state, frame([structuredClone(valid)])), state);
  const failed = apply(state, frame([{ ...valid, exit_code: 1 }]));
  assert.notEqual(failed, state);
  assert.equal(activeChannel(failed).terminalRuns[0]?.exitCode, 1);
  assert.equal(activeChannel(failed).messages, activeChannel(state).messages);
});

function landingFrame(
  key: string,
  name: string,
  { needsYou = false, at = 100 }: { needsYou?: boolean; at?: number } = {},
) {
  const value = snapshot(key, name, `w-${name}`);
  return {
    ...value,
    frame: {
      ...value.frame,
      health: needsYou ? { level: "needs_you", items: 1 } : value.frame.health,
      messages: value.frame.messages.map((row) => ({ ...row, at_ms: at })),
    },
  };
}
function landed(
  frames: ReturnType<typeof landingFrame>[],
  requestedKey: string | null = null,
  end: "ready" | "timeout" = "ready",
) {
  let state = reduce(
    initialState({
      requestedKey:
        requestedKey === null ? null : parsed(snapshot(requestedKey)).key,
    }),
    { type: "opened" },
  );
  for (const frame of frames) state = apply(state, frame);
  return end === "ready"
    ? ready(
        state,
        frames.map((frame) => frame.key),
      )
    : reduce(state, { type: "replay-timeout" });
}
test("design-b: landing prefers the URL key, then needs you in sidebar order, then the latest message", () => {
  const keyC = "c".repeat(20);
  const keyD = "d".repeat(20);
  const alpha = landingFrame(keyA, "alpha", { at: 300 });
  const beta = landingFrame(keyD, "beta", { at: 900 });
  const frames = [
    alpha,
    landingFrame(keyB, "zeta", { needsYou: true }),
    landingFrame(keyC, "mu", { needsYou: true }),
    beta,
  ];
  for (const end of ["ready", "timeout"] as const) {
    assert.equal(landed(frames, keyA, end).selection.key, keyA);
    assert.equal(landed(frames, null, end).selection.key, keyC);
    assert.equal(landed(frames, "e".repeat(20), end).selection.key, keyC);
    assert.equal(landed([alpha, beta], null, end).selection.key, keyD);
    assert.equal(
      landed(
        [landingFrame(keyB, "zeta"), landingFrame(keyA, "alpha")],
        null,
        end,
      ).selection.key,
      keyA,
    );
  }
  const chosen = reduce(landed(frames), {
    type: "select",
    key: parsed(snapshot(keyD)).key,
  });
  const reconnect = ready(
    frames.reduce(
      (state, frame) => apply(state, frame),
      reduce(chosen, { type: "opened" }),
    ),
    frames.map((frame) => frame.key),
  );
  assert.equal(reconnect.selection.key, keyD);
});
test("design-b: switching channels closes the open thread", () => {
  const state = twoChannels();
  const alpha = parsed(snapshot()).key;
  const beta = parsed(snapshot(keyB)).key;
  assert.equal(state.selection.key, alpha);
  const open = reduce(state, { type: "thread", threadId: "m1" });
  assert.equal(open.openThread, "m1");
  assert.equal(reduce(open, { type: "thread", threadId: "m1" }), open);
  assert.equal(reduce(open, { type: "select", key: alpha }).openThread, "m1");
  assert.equal(reduce(open, { type: "select", key: beta }).openThread, null);
  assert.equal(reduce(open, { type: "gone", key: alpha }).openThread, null);
  assert.equal(
    reduce(open, { type: "thread", threadId: null }).openThread,
    null,
  );
  assert.equal(apply(open, changedChannel()).openThread, "m1");
});
test("a thread whose messages all left the window closes for good, one still loaded stays open", () => {
  const paged = historyPage(apply(initialState(), withMessages(["a", "b"])), [
    "h1",
    "h2",
  ]);
  assert.equal(activeChannel(paged).messages.size, 4);
  const key = activeChannel(paged).key;
  const open = reduce(paged, { type: "thread", threadId: "h1" });
  assert.equal(open.openThread, "h1");
  const jumped = reduce(open, { type: "latest", key });
  assert.equal(activeChannel(jumped).messages.has("h1"), false);
  assert.equal(jumped.openThread, null);
  assert.equal(historyPage(jumped, ["h1", "h2"]).openThread, null);
  const kept = reduce(reduce(paged, { type: "thread", threadId: "a" }), {
    type: "latest",
    key,
  });
  assert.equal(kept.openThread, "a");
});
test("landing: a late URL key is selected when it arrives, and from then on nothing moves by itself", () => {
  const keyC = "c".repeat(20);
  const fallback = reduce(
    reduce(initialState({ requestedKey: parsed(snapshot(keyB)).key }), {
      type: "opened",
    }),
    { type: "replay-timeout" },
  );
  const alpha = apply(fallback, landingFrame(keyA, "alpha"));
  assert.equal(alpha.selection.key, keyA);
  const requested = apply(alpha, landingFrame(keyB, "beta"));
  assert.equal(requested.selection.key, keyB);
  const later = apply(
    requested,
    landingFrame(keyC, "gamma", { needsYou: true }),
  );
  assert.equal(later.selection.key, keyB);
  assert.equal(ready(later, [keyA, keyB, keyC]).selection.key, keyB);
});
test("landing: without a URL key each new channel re-runs the rule, updates never move it, and a pick ends it", () => {
  const keyC = "c".repeat(20);
  const keyD = "d".repeat(20);
  const fallback = reduce(reduce(initialState(), { type: "opened" }), {
    type: "replay-timeout",
  });
  const alpha = apply(fallback, landingFrame(keyA, "alpha"));
  assert.equal(alpha.selection.key, keyA);
  const needsYou = apply(
    alpha,
    landingFrame(keyC, "gamma", { needsYou: true }),
  );
  assert.equal(needsYou.selection.key, keyC);
  const updated = apply(
    needsYou,
    landingFrame(keyA, "alpha", { needsYou: true }),
  );
  assert.equal(updated.selection.key, keyC);
  assert.equal(ready(updated, [keyA, keyC]).selection.key, keyC);
  const chosen = reduce(updated, {
    type: "select",
    key: parsed(snapshot(keyA)).key,
  });
  const delta = landingFrame(keyD, "delta", { needsYou: true });
  assert.equal(apply(chosen, delta).selection.key, keyA);
  assert.equal(
    ready(apply(chosen, delta), [keyA, keyC, keyD]).selection.key,
    keyA,
  );
});
test("landing: a reconnect replay never moves the selection or closes the thread, even when it brings a channel that needs you", () => {
  const alpha = landingFrame(keyA, "alpha");
  const state = reduce(landed([alpha]), { type: "thread", threadId: "m1" });
  assert.equal(state.selection.key, keyA);
  const known = ready(
    apply(
      reduce(state, { type: "opened" }),
      landingFrame(keyA, "alpha", { needsYou: true }),
    ),
    [keyA],
  );
  assert.equal(known.selection.key, keyA);
  assert.equal(known.openThread, "m1");
  const grown = ready(
    apply(
      apply(reduce(state, { type: "opened" }), alpha),
      landingFrame(keyB, "beta", { needsYou: true }),
    ),
    [keyA, keyB],
  );
  assert.equal(grown.selection.key, keyA);
  assert.equal(grown.openThread, "m1");
});
test("landing: after ready, a workspace that opens later never moves an automatic landing or closes its thread", () => {
  const keyC = "c".repeat(20);
  const keyD = "d".repeat(20);
  const state = reduce(
    landed([
      landingFrame(keyA, "alpha", { at: 100 }),
      landingFrame(keyB, "beta", { at: 200 }),
    ]),
    { type: "thread", threadId: "m1" },
  );
  assert.equal(state.selection.key, keyB);
  const calm = apply(state, landingFrame(keyC, "gamma", { at: 900 }));
  assert.equal(calm.selection.key, keyB);
  assert.equal(calm.openThread, "m1");
  const needsYou = apply(calm, landingFrame(keyD, "delta", { needsYou: true }));
  assert.equal(needsYou.selection.key, keyB);
  assert.equal(needsYou.openThread, "m1");
  assert.equal(needsYou.channels.size, 4);
});
test("navigate: Back to the root lands once more, and after the reader's pick nothing moves it again", () => {
  const keyC = "c".repeat(20);
  const keyD = "d".repeat(20);
  const betaKey = parsed(snapshot(keyB)).key;
  const state = landed([
    landingFrame(keyA, "alpha"),
    landingFrame(keyB, "beta"),
  ]);
  const chosen = reduce(state, { type: "select", key: betaKey });
  const root = reduce(chosen, { type: "navigate", key: null });
  assert.equal(root.selection.key, keyA);
  const gamma = apply(root, landingFrame(keyC, "gamma", { needsYou: true }));
  assert.equal(gamma.selection.key, keyA);
  const forward = reduce(gamma, { type: "navigate", key: betaKey });
  assert.equal(forward.selection.key, keyB);
  const delta = landingFrame(keyD, "delta", { needsYou: true });
  assert.equal(apply(forward, delta).selection.key, keyB);
  assert.equal(
    reduce(forward, {
      type: "navigate",
      key: parsed(snapshot("e".repeat(20))).key,
    }),
    forward,
  );
});
test("select: the pick replaces the URL's key, so a lost pick lands by Needs you instead of the load-time URL", () => {
  const keyC = "c".repeat(20);
  const betaKey = parsed(snapshot(keyB)).key;
  const state = landed(
    [landingFrame(keyA, "alpha"), landingFrame(keyB, "beta")],
    keyA,
  );
  assert.equal(state.selection.key, keyA);
  const lost = reduce(reduce(state, { type: "select", key: betaKey }), {
    type: "gone",
    key: betaKey,
  });
  assert.equal(lost.selection.key, null);
  assert.equal(
    apply(lost, landingFrame(keyC, "gamma", { needsYou: true })).selection.key,
    keyC,
  );
});
test("landing: reader input ends discovery, so a channel that needs you no longer moves the selection", () => {
  const keyC = "c".repeat(20);
  const fallback = reduce(reduce(initialState(), { type: "opened" }), {
    type: "replay-timeout",
  });
  const touched = reduce(apply(fallback, landingFrame(keyA, "alpha")), {
    type: "input",
  });
  assert.equal(touched.selection.key, keyA);
  const gamma = apply(touched, landingFrame(keyC, "gamma", { needsYou: true }));
  assert.equal(gamma.selection.key, keyA);
  assert.equal(ready(gamma, [keyA, keyC]).selection.key, keyA);
  assert.equal(reduce(gamma, { type: "input" }), gamma);
});
test("landing: a server without ready ends discovery on the timeout, after which only the URL's channel is chosen", () => {
  const keyC = "c".repeat(20);
  const fallback = reduce(
    reduce(initialState({ requestedKey: parsed(snapshot(keyB)).key }), {
      type: "opened",
    }),
    { type: "replay-timeout" },
  );
  const timedOut = reduce(apply(fallback, landingFrame(keyA, "alpha")), {
    type: "discovery-timeout",
  });
  assert.equal(timedOut.selection.key, keyA);
  assert.equal(reduce(timedOut, { type: "discovery-timeout" }), timedOut);
  const gamma = apply(
    timedOut,
    landingFrame(keyC, "gamma", { needsYou: true }),
  );
  assert.equal(gamma.selection.key, keyA);
  assert.deepEqual(apply(gamma, landingFrame(keyB, "beta")).selection, {
    kind: "chosen",
    key: keyB,
  });
});
test("landing: after ready the URL's channel is chosen when it arrives, unless the reader acted first", () => {
  const start = reduce(
    initialState({ requestedKey: parsed(snapshot(keyB)).key }),
    { type: "opened" },
  );
  const state = ready(apply(start, landingFrame(keyA, "alpha")), [keyA]);
  assert.equal(state.selection.key, keyA);
  assert.deepEqual(apply(state, landingFrame(keyB, "beta")).selection, {
    kind: "chosen",
    key: keyB,
  });
  const touched = reduce(state, { type: "input" });
  assert.equal(apply(touched, landingFrame(keyB, "beta")).selection.key, keyA);
});

const MIN = 60_000;
type RowOptions = {
  author?: string;
  to?: string[];
  kind?: string;
  thread?: string;
  inReplyTo?: string | null;
};
function row(
  id: string,
  at: number,
  {
    author = "agent-1",
    to = [],
    kind = "mention",
    thread = id,
    inReplyTo = null,
  }: RowOptions = {},
) {
  return {
    ...message(id),
    thread_id: thread,
    in_reply_to: inReplyTo,
    at_ms: at,
    author,
    kind,
    legs: to.map((address) => ({ to: address, stage: { stage: "queued" } })),
  };
}
function view(rows: ReturnType<typeof row>[], now: number) {
  const value = snapshot();
  value.frame.messages = rows;
  return timelineView(
    activeChannel(apply(initialState(), value)),
    undefined,
    now,
  );
}
function summary(rows: ReturnType<typeof view>) {
  return rows.map((item) =>
    item.kind === "day" || item.kind === "gap"
      ? item.kind === "day"
        ? `day:${item.label}`
        : "gap"
      : `${item.message.id}${item.grouped ? " grouped" : ""}${item.showRecipients ? " to" : ""}`,
  );
}
test("design-b: timeline groups by author within five minutes and never across a day", () => {
  const today = new Date(2026, 9, 28, 0, 1).getTime();
  const yesterday = new Date(2026, 9, 27, 23, 58).getTime();
  const rows = view(
    [
      row("y1", yesterday, { to: ["agent-2"] }),
      row("t1", today, { to: ["agent-2"] }),
      row("t2", today + 4 * MIN, { to: ["agent-2"] }),
      row("t3", today + 9 * MIN + 1, { to: ["agent-2"] }),
      row("t4", today + 10 * MIN, { to: ["agent-2", "agent-2"] }),
      row("t5", today + 11 * MIN, { to: ["shell"] }),
      row("t6", today + 12 * MIN, { to: ["shell"], kind: "handoff" }),
      row("t7", today + 13 * MIN, { author: "human", to: ["shell"] }),
      row("t7b", today + 13 * MIN + 30_000, { author: "human" }),
      row("t8", today + 14 * MIN, { author: "human", kind: "nudge" }),
      row("t9", today + 15 * MIN, { author: "human" }),
    ],
    today + 20 * MIN,
  );
  assert.deepEqual(summary(rows), [
    "day:Yesterday",
    "y1 to",
    "day:Today",
    "t1 to",
    "t2 grouped",
    "t3 to",
    "t4 grouped",
    "t5 grouped to",
    "t6 grouped to",
    "t7 to",
    "t7b grouped",
    "t8 to",
    "t9 to",
  ]);
  const t4 = rows.find(
    (item) => item.kind === "message" && item.message.id === "t4",
  );
  assert.ok(t4?.kind === "message");
  assert.deepEqual(t4.recipients, ["agent-2"]);
  const again = view([row("t4", today, { to: ["agent-2", "agent-2"] })], today);
  assert.ok(again[1]?.kind === "message");
  assert.deepEqual(again[1].recipients, ["agent-2"]);
});
test("design-b: request rows carry their latest reply and count, and replies stay in the thread", () => {
  const now = new Date(2026, 9, 28, 12, 0).getTime();
  const rows = view(
    [
      row("ask", now - 10 * MIN, { to: ["agent-2"], thread: "t" }),
      row("first", now - 9 * MIN, {
        author: "agent-2",
        kind: "reply",
        thread: "t",
        inReplyTo: "ask",
      }),
      row("lone", now - 8 * MIN),
      row("second", now - 7 * MIN, {
        author: "agent-2",
        kind: "reply",
        thread: "t",
        inReplyTo: "ask",
      }),
    ],
    now,
  );
  const messages = rows.filter(
    (item): item is MessageRow => item.kind === "message",
  );
  assert.deepEqual(
    messages.map((item) => [
      item.message.id,
      item.latestReply?.id ?? null,
      item.replyCount,
    ]),
    [
      ["ask", "second", 2],
      ["lone", null, 0],
    ],
  );
  assert.equal(
    messages[1]?.grouped,
    true,
    "hidden replies do not break the author's group",
  );
});
test("design-b: a gap row resets grouping and keeps the day", () => {
  const now = new Date(2026, 9, 28, 12, 0).getTime();
  let state = apply(initialState(), {
    ...snapshot(),
    frame: { ...snapshot().frame, messages: [row("a", now - 3 * MIN)] },
  });
  state = apply(state, {
    ...snapshot(),
    frame: { ...snapshot().frame, messages: [row("b", now - 2 * MIN)] },
  });
  assert.deepEqual(
    summary(timelineView(activeChannel(state), undefined, now)),
    ["day:Today", "a to", "gap", "b to"],
  );
});
test("design-b fix: a frame keeps per-row props identical for unchanged messages", () => {
  const now = new Date(2026, 9, 28, 12, 0).getTime();
  const ask = row("ask", now - 3 * MIN, { to: ["agent-2"] });
  const value = snapshot();
  value.frame.messages = [ask];
  const state = apply(initialState(), value);
  value.frame.messages = [ask, row("next", now - MIN, { to: ["agent-2"] })];
  const next = apply(state, value);
  const askRow = (rows: readonly TimelineRow[]) =>
    rows.find(
      (item): item is MessageRow =>
        item.kind === "message" && item.message.id === "ask",
    );
  const first = askRow(timelineView(activeChannel(state), undefined, now));
  const second = askRow(timelineView(activeChannel(next), undefined, now));
  assert.ok(first && second);
  assert.deepEqual(first.recipients, ["agent-2"]);
  assert.equal(second.message, first.message);
  assert.equal(second.recipients, first.recipients);
});
test("design-b fix: a new terminal run keeps unchanged runs identical", () => {
  const run = (message_id: string, exit_code: number) => ({
    terminal: "shell-1",
    command: { text: "echo hi", truncated: false },
    started_at_ms: 90,
    exit_code,
    message_id,
  });
  const value = snapshot();
  const state = apply(initialState(), {
    ...value,
    frame: { ...value.frame, terminal_runs: [run("m1", 0)] },
  });
  const next = apply(state, {
    ...value,
    frame: { ...value.frame, terminal_runs: [run("m1", 0), run("m2", 1)] },
  });
  const before = runsByMessage(activeChannel(state).terminalRuns);
  const after = runsByMessage(activeChannel(next).terminalRuns);
  assert.equal(after.get("m2")?.[0]?.exitCode, 1);
  assert.ok(before.get("m1")?.[0]);
  assert.equal(after.get("m1")?.[0], before.get("m1")?.[0]);
});
test("design-b: each terminal reply gets the latest run that started at or before it, whatever the list order", () => {
  const run = (
    exit_code: number,
    started_at_ms: number,
    terminal = "shell-1",
  ) => ({
    terminal,
    command: { text: "make", truncated: false },
    started_at_ms,
    exit_code,
    message_id: "m1",
  });
  const reply = (message_id: string, at_ms: number) => ({
    ...message(message_id),
    thread_id: "m1",
    in_reply_to: "m1",
    kind: "reply",
    author: "shell-1",
    at_ms,
  });
  const value = snapshot();
  value.frame.messages = [
    message("m1"),
    reply("r0", 500),
    reply("r1", 1500),
    reply("r2", 2500),
  ];
  const channel = activeChannel(
    apply(initialState(), {
      ...value,
      frame: {
        ...value.frame,
        terminal_runs: [run(7, 1200, "shell-2"), run(0, 2000), run(1, 1000)],
      },
    }),
  );
  const runs = runsByMessage(channel.terminalRuns);
  const exit = (id: string) => {
    const row = channel.messages.get(id);
    assert.ok(row);
    return exitFor(runs, row)?.exitCode;
  };
  assert.equal(exit("r1"), 1);
  assert.equal(exit("r2"), 0);
  assert.equal(exit("r0"), undefined);
  assert.equal(exit("m1"), undefined);
});

function outgoing(requestId = "r1", source = "@codex hi") {
  return {
    requestId,
    key: parsed(snapshot()).key,
    source,
    inReplyTo: null,
    at: 1,
  };
}
function phases(state: State): Record<string, string> {
  return Object.fromEntries(
    [...state.outbox].map(([id, entry]) => [id, entry.phase.kind]),
  );
}
test("outbox: a send is pending until accepted, then a frame carrying its row drops it", () => {
  const ready = apply(initialState(), snapshot());
  const sending = reduce(ready, { type: "send", outgoing: outgoing() });
  assert.deepEqual(phases(sending), { r1: "sending" });
  assert.equal(
    reduce(sending, { type: "send", outgoing: outgoing() }),
    sending,
    "a second send with the same id while sending changes nothing",
  );
  const sent = reduce(sending, {
    type: "sent",
    requestId: "r1",
    messageId: "posted",
  });
  assert.deepEqual(phases(sent), { r1: "sent" });
  const unrelated = snapshot();
  unrelated.frame.messages.push(message("other"));
  assert.deepEqual(phases(apply(sent, unrelated)), { r1: "sent" });
  const arrived = snapshot();
  arrived.frame.messages.push({ ...message("posted"), author: "human" });
  const settled = apply(sent, arrived);
  assert.deepEqual(phases(settled), {});
  assert.ok(settled.channels.get(parsed(arrived).key)?.messages.has("posted"));
});
test("outbox: a row that lands in another channel still settles the send", () => {
  const state = reduce(twoChannels(), { type: "send", outgoing: outgoing() });
  const sent = reduce(state, {
    type: "sent",
    requestId: "r1",
    messageId: "cross",
  });
  const beta = snapshot(keyB, "beta", "w2");
  beta.frame.messages.push(message("cross"));
  assert.deepEqual(phases(apply(sent, beta)), {});
});
test("outbox: an acceptance that arrives after its row settles at once", () => {
  const arrived = snapshot();
  arrived.frame.messages.push(message("early"));
  const state = reduce(apply(initialState(), snapshot()), {
    type: "send",
    outgoing: outgoing(),
  });
  const withRow = apply(state, arrived);
  assert.deepEqual(phases(withRow), { r1: "sending" });
  assert.deepEqual(
    phases(
      reduce(withRow, { type: "sent", requestId: "r1", messageId: "early" }),
    ),
    {},
  );
});
test("outbox: replayed rows settle a send only when the replay commits", () => {
  const sent = reduce(
    reduce(apply(initialState(), snapshot()), {
      type: "send",
      outgoing: outgoing(),
    }),
    { type: "sent", requestId: "r1", messageId: "replayed" },
  );
  const replaying = reduce(sent, { type: "opened" });
  const arrived = snapshot();
  arrived.frame.messages.push(message("replayed"));
  const staged = apply(replaying, arrived);
  assert.deepEqual(phases(staged), { r1: "sent" });
  const committed = reduce(staged, {
    type: "ready",
    keys: new Set([parsed(arrived).key]),
  });
  assert.deepEqual(phases(committed), {});
});
test("outbox: a refused send keeps its id for an identical retry and only then", () => {
  const failed = reduce(
    reduce(apply(initialState(), snapshot()), {
      type: "send",
      outgoing: outgoing(),
    }),
    { type: "send-failed", requestId: "r1", status: 400, message: "Nope" },
  );
  assert.deepEqual(failed.outbox.get("r1")?.phase, {
    kind: "failed",
    status: 400,
    message: "Nope",
  });
  const draft = outgoing();
  assert.equal(retryId(failed.outbox, draft), "r1");
  assert.equal(retryId(failed.outbox, { ...draft, source: "changed" }), null);
  assert.equal(retryId(failed.outbox, { ...draft, inReplyTo: "m1" }), null);
  assert.equal(
    retryId(failed.outbox, { ...draft, key: parsed(snapshot(keyB)).key }),
    null,
  );
  assert.equal(
    reduce(failed, { type: "sent", requestId: "r1", messageId: "late" }),
    failed,
    "a late result for a refused send changes nothing",
  );
  const retried = reduce(failed, {
    type: "send",
    outgoing: { ...draft, at: 2 },
  });
  assert.deepEqual(phases(retried), { r1: "sending" });
  assert.equal(retried.outbox.get("r1")?.at, 2);
  assert.equal(
    reduce(retried, { type: "sent", requestId: "missing", messageId: "x" }),
    retried,
  );
});
test("participants: envoys resolve through their home channel, else read in plain words", () => {
  const alpha = parsed(snapshot(keyA, "alpha", "w1"));
  const home = snapshot(keyB, "beta", "w2");
  home.frame.participants = [{ ...participant("pane-9"), alias: "@reviewer" }];
  const beta = parsed(home);
  const homes = homesOf([alpha, beta], undefined);
  assert.equal(homesOf([alpha, beta], homes), homes, "unchanged homes reuse");
  const directory = { members: alpha.members, homes };
  const id = (raw: string) => {
    const [leg] =
      parsed({
        ...snapshot(),
        frame: {
          ...snapshot().frame,
          messages: [
            { ...message(), legs: [{ to: raw, stage: { stage: "queued" } }] },
          ],
        },
      }).messages.get("m1")?.legs ?? [];
    assert.ok(leg);
    return leg.to;
  };
  assert.deepEqual(resolve(directory, "human"), { kind: "operator" });
  assert.equal(resolve(directory, id("agent-1")).kind, "member");
  const remote = resolve(directory, id("envoy:w2:pane-9"));
  assert.ok(remote.kind === "member");
  assert.equal(remote.member.alias, "@reviewer");
  assert.equal(remote.home, "#beta");
  assert.deepEqual(resolve(directory, id("envoy:w2:pane-gone")), {
    kind: "envoy",
    id: "envoy:w2:pane-gone",
    home: "#beta",
  });
  assert.deepEqual(resolve(directory, id("envoy:w7:pane-9")), {
    kind: "envoy",
    id: "envoy:w7:pane-9",
    home: null,
  });
  assert.deepEqual(resolve(directory, id("stranger")), {
    kind: "unknown",
    id: "stranger",
  });
});
test("thread recipients: the agents who wrote, else the parent's recipients, never the operator", () => {
  const operator: unknown = {
    ...participant("human"),
    alias: "you",
    kind: { type: "human" },
  };
  const parent = {
    ...message("parent"),
    author: "human",
    legs: [{ to: "agent-2", stage: { stage: "queued" } }],
  };
  const answer = {
    ...message("answer"),
    thread_id: "parent",
    in_reply_to: "parent",
    kind: "reply",
    author: "agent-1",
  };
  const thread = (messages: unknown[]) => {
    const value = snapshot();
    const channel = parsed({
      ...value,
      frame: {
        ...value.frame,
        participants: [
          participant("agent-1"),
          { ...participant("agent-2"), alias: "@claude" },
          operator,
        ],
        messages,
      },
    });
    const group = threadGroups({
      ...channel,
      runs: [[...channel.messages.keys()]],
    }).get("parent");
    assert.ok(group);
    return replyRecipients(group, {
      members: channel.members,
      homes: new Map(),
    });
  };
  assert.deepEqual(thread([parent]), ["@claude"]);
  assert.deepEqual(thread([parent, answer]), ["@codex"]);
});
