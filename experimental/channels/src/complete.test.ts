import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accept,
  type Candidate,
  type Completion,
  type CompletionContext,
  channelOptions,
  complete,
  leadingMention,
} from "./complete.ts";
import type { ChannelSnapshot, SpaceKey } from "./model.ts";
import { parseEvent } from "./wire.ts";

function participant(
  alias: string,
  kind: unknown = { type: "agent", runtime: "codex" },
  activity = "idle",
) {
  return {
    address_id: `id-${alias}`,
    alias,
    kind,
    activity,
    role: null,
    pane_label: null,
  };
}
function snapshot(
  letter: string,
  name: string,
  label: string | null,
  participants: unknown[] = [],
  workspace = `w-${letter}`,
): ChannelSnapshot {
  const key = letter.repeat(20);
  const event = parseEvent(
    "space",
    JSON.stringify({
      key,
      frame: {
        wire: 1,
        space: {
          key,
          workspace_id: workspace,
          display_name: name,
          addressable_label: label,
        },
        health: { level: "calm" },
        participants,
        messages: [],
        threads: [],
        attention: [],
      },
    }),
  );
  assert.ok(event?.type === "space");
  return event.channel;
}
const snapshots = [
  snapshot("b", "beta", "beta", [
    participant("@coder"),
    participant("@reviewer"),
  ]),
  snapshot("a", "alpha", "alpha", [
    participant("@codex"),
    participant("@claude"),
    participant("you", { type: "human" }),
    participant("@ghost", { type: "agent", runtime: null }, "retired"),
    participant("@shell", { type: "terminal" }),
  ]),
  snapshot("c", "shared", null),
  snapshot("d", "w-d", null),
];
const [beta, alpha] = snapshots;
assert.ok(alpha && beta);
const alphaKey: SpaceKey = alpha.key;
const betaKey: SpaceKey = beta.key;
const byKey = new Map(snapshots.map((channel) => [channel.key, channel]));
const context: CompletionContext = {
  current: alphaKey,
  channels: channelOptions(snapshots),
  membersOf: (key) => [...(byKey.get(key)?.members.values() ?? [])],
};
function names(completion: Completion | null): string[] {
  assert.ok(completion);
  return completion.candidates.map((candidate) => {
    switch (candidate.kind) {
      case "channel":
        return candidate.channel.name;
      case "all":
        return "#all";
      case "member":
        return candidate.member.alias;
      default: {
        const exhaustive: never = candidate;
        return exhaustive;
      }
    }
  });
}
function pick(completion: Completion | null, name: string): Candidate {
  assert.ok(completion);
  const found = completion.candidates.find(
    (candidate) =>
      (candidate.kind === "channel" && candidate.channel.name === name) ||
      (candidate.kind === "member" && candidate.member.alias === name) ||
      (candidate.kind === "all" && name === "#all"),
  );
  assert.ok(found, `${name} is a candidate`);
  return found;
}

test("complete: a caret in the middle of a word completes the whole word around it", () => {
  const text = "ping #alp now";
  const completion = complete(text, "ping #a".length, context);
  assert.deepEqual(completion?.token, {
    kind: "channel",
    start: 5,
    end: 9,
    query: "alp",
  });
  assert.deepEqual(names(completion), ["alpha"]);
  assert.ok(completion);
  assert.deepEqual(accept(text, completion, pick(completion, "alpha")), {
    text: "ping #alpha now",
    caret: "ping #alpha ".length,
  });
});

test("complete: an @ after a #channel lists that channel's members, else the current channel's", () => {
  assert.deepEqual(names(complete("#beta @", 7, context)), [
    "@coder",
    "@reviewer",
  ]);
  assert.equal(complete("#beta @", 7, context)?.target, betaKey);
  assert.deepEqual(names(complete("#beta #alpha @c", 15, context)), [
    "@claude",
    "@codex",
  ]);
  assert.equal(complete("@", 1, context)?.target, alphaKey);
  assert.deepEqual(names(complete("#nowhere @", 10, context)), [
    "@claude",
    "@codex",
    "@shell",
  ]);
  assert.deepEqual(
    names(complete("#beta @co", 9, context)),
    ["@coder"],
    "a channel token before the caret wins over the current channel",
  );
});

test("complete: the member list leaves out the operator and retired agents", () => {
  const listed = names(complete("@", 1, context));
  assert.deepEqual(listed, ["@claude", "@codex", "@shell"]);
  assert.equal(listed.includes("you"), false);
  assert.equal(listed.includes("@ghost"), false);
});

test("complete: a channel without a token is listed but accept refuses it", () => {
  const completion = complete("#", 1, context);
  assert.deepEqual(names(completion), [
    "alpha",
    "beta",
    "shared",
    "w-d",
    "#all",
  ]);
  assert.ok(completion);
  assert.equal(accept("#", completion, pick(completion, "shared")), null);
  assert.equal(accept("#", completion, pick(completion, "w-d")), null);
  assert.deepEqual(accept("#", completion, pick(completion, "#all")), {
    text: "#all ",
    caret: 5,
  });
});

test("complete: an @ inside a word, as in an email address, never opens the list", () => {
  assert.equal(complete("mail ops@codex", 14, context), null);
  assert.equal(complete("ops@", 4, context), null);
  assert.equal(complete("tag#alpha", 9, context), null);
  assert.equal(complete("@codex ", 7, context), null, "caret after a space");
  assert.equal(complete("@codex", 0, context), null, "caret before the @");
  assert.ok(complete("hi\n@co", 6, context), "a newline separates words");
});

test("complete: prefix matches rank before substring matches, ties by name, ignoring case", () => {
  const crowd = snapshot("e", "crowd", "crowd", [
    participant("@Decoder"),
    participant("@coder"),
    participant("@Codex"),
    participant("@encode"),
  ]);
  const ranked: CompletionContext = {
    ...context,
    membersOf: () => [...crowd.members.values()],
  };
  assert.deepEqual(names(complete("@CO", 3, ranked)), [
    "@coder",
    "@Codex",
    "@Decoder",
    "@encode",
  ]);
  assert.deepEqual(names(complete("#ta", 3, context)), ["beta"]);
  assert.deepEqual(names(complete("#l", 2, context)), ["alpha", "#all"]);
});

test("complete: #all comes last and accepting a member replaces the token and adds one space", () => {
  assert.deepEqual(names(complete("#a", 2, context)).at(-1), "#all");
  const text = "#al";
  const first = complete(text, 3, context);
  assert.ok(first);
  const channel = accept(text, first, pick(first, "alpha"));
  assert.deepEqual(channel, { text: "#alpha ", caret: 7 });
  const typed = `${channel?.text}@co`;
  const second = complete(typed, typed.length, context);
  assert.ok(second);
  assert.deepEqual(accept(typed, second, pick(second, "@codex")), {
    text: "#alpha @codex ",
    caret: 14,
  });
  const middle = "@co rest";
  const third = complete(middle, 2, context);
  assert.ok(third);
  assert.deepEqual(
    accept(middle, third, pick(third, "@codex")),
    { text: "@codex rest", caret: 7 },
    "an existing space after the token is reused",
  );
});

test("complete: channel options mark shared and missing labels from the snapshot", () => {
  assert.deepEqual(
    channelOptions(snapshots).map(({ name, token, disabledReason }) => [
      name,
      token,
      disabledReason,
    ]),
    [
      ["beta", "#beta", null],
      ["alpha", "#alpha", null],
      ["shared", null, "shared-label"],
      ["w-d", null, "no-label"],
    ],
  );
});

test("complete: a post names its recipients only the way the server's grammar reads them", () => {
  const cases: [string, boolean][] = [
    ["@codex hi", true],
    ["  #beta @codex hi", true],
    ["#all @codex hi", true],
    ["#alpha #beta @lead @reviewer hi", true],
    ["@codex\nhi", true],
    ["@codex", true],
    ["hello", false],
    ["hi @codex", false],
    ["@ alone", false],
    ["#alpha hi", false],
    ["#alpha", false],
    ["#42 fix the build", false],
    ["@Codex hi", false],
    ["@codex, hi", false],
    ["@-codex hi", false],
    ["@co--dex hi", false],
    ["#alpha hi @codex", false],
  ];
  for (const [text, named] of cases)
    assert.equal(leadingMention(text), named, JSON.stringify(text));
});
