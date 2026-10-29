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
  postSource,
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
  state: string | null = "active",
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
          ...(state === null ? {} : { state }),
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
  snapshot("c", "dormant", null),
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
const project = snapshot("1", "Alpha Project", "alpha-project");
const withProject: CompletionContext = {
  ...context,
  channels: channelOptions([...snapshots, project]),
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
    "dormant",
    "w-d",
    "#all",
  ]);
  assert.ok(completion);
  assert.equal(accept("#", completion, pick(completion, "dormant")), null);
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

test("complete: a # query matches the typeable token as well as the name, ignoring case", () => {
  const listed = (text: string) =>
    names(complete(text, text.length, withProject));
  assert.deepEqual(listed("#alpha-p"), ["Alpha Project"]);
  assert.deepEqual(listed("#ALPHA"), ["alpha", "Alpha Project"]);
  assert.deepEqual(listed("#proj"), ["Alpha Project"]);
  assert.deepEqual(listed("#-pro"), ["Alpha Project"]);
  assert.deepEqual(
    listed("#DORM"),
    ["dormant"],
    "a channel without a token still matches by name",
  );
});

test("complete: the #token before an @ resolves by folding, so #Beta @ lists beta's members", () => {
  assert.deepEqual(names(complete("#Beta @", 7, context)), [
    "@coder",
    "@reviewer",
  ]);
  assert.equal(complete("#Beta @", 7, context)?.target, betaKey);
  assert.deepEqual(names(complete("#BETA @co", 9, context)), ["@coder"]);
  assert.deepEqual(names(complete("#Alpha-Project @", 16, withProject)), []);
  assert.equal(
    complete("#Alpha-Project @", 16, withProject)?.target,
    project.key,
  );
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

test("complete: channel options name why a channel cannot be mentioned, in the table's order", () => {
  assert.deepEqual(
    channelOptions(snapshots).map(({ name, token, disabledReason }) => [
      name,
      token,
      disabledReason,
    ]),
    [
      ["beta", "#beta", null],
      ["alpha", "#alpha", null],
      ["dormant", null, "unknown"],
      ["w-d", null, "no-label"],
    ],
  );
  const reasons = channelOptions([
    snapshot("2", "w-2", null),
    snapshot("3", "日本語", null),
    snapshot("4", "All", null),
    snapshot("5", "Twin", null),
    snapshot("6", "twin", null),
    snapshot("7", "Gamma", "gamma"),
    snapshot("8", "gamma!", null),
    snapshot("9", "Dormant", null),
    snapshot("a", "w-2", null),
    snapshot("b", "foobar", "foobar"),
    snapshot("c", "foobar", null),
    snapshot("d", "same", null),
    snapshot("e", "same", null),
    snapshot("f", "日本語", null),
    snapshot("1", "solo", "solo"),
    snapshot("0", "solo", null),
  ]).map(({ name, disabledReason }) => [name, disabledReason]);
  assert.deepEqual(reasons, [
    ["w-2", "no-label"],
    ["日本語", "unusable-label"],
    ["All", "unknown"],
    ["Twin", "unknown"],
    ["twin", "unknown"],
    ["Gamma", null],
    ["gamma!", "unknown"],
    ["Dormant", "unknown"],
    ["w-2", "unknown"],
    ["foobar", null],
    ["foobar", "unknown"],
    ["same", "shared-name"],
    ["same", "shared-name"],
    ["日本語", "unusable-label"],
    ["solo", null],
    ["solo", "unknown"],
  ]);
});

test("complete: twins named all read the general line, and a shared name counts only while every twin is active", () => {
  const reasons = (...channels: ChannelSnapshot[]) =>
    channelOptions(channels).map(({ disabledReason }) => disabledReason);
  const twin = (letter: string, name: string, state: string | null) =>
    snapshot(letter, name, null, [], `w-${letter}`, state);
  assert.deepEqual(
    reasons(twin("1", "all", "active"), twin("2", "all", "active")),
    ["unknown", "unknown"],
  );
  assert.deepEqual(
    reasons(twin("1", "twin", "paused"), twin("2", "twin", "active")),
    ["shared-name", "shared-name"],
  );
  for (const state of ["deactivated", "not_activated", "unknown", null])
    assert.deepEqual(
      reasons(twin("1", "twin", "active"), twin("2", "twin", state)),
      ["unknown", "unknown"],
      `a twin whose state is ${state}`,
    );
});
test("complete: channel options that share a name carry the sidebar's workspace suffix", () => {
  const e = "e".repeat(20);
  const f = "f".repeat(20);
  const nine = "9".repeat(20);
  assert.deepEqual(
    channelOptions([
      snapshot("e", "twin", null),
      snapshot("f", "twin", null),
      snapshot("9", "solo", "solo"),
    ]),
    [
      {
        key: e,
        name: "twin",
        token: null,
        disabledReason: "shared-name",
        suffix: `w-e · ${e}`,
      },
      {
        key: f,
        name: "twin",
        token: null,
        disabledReason: "shared-name",
        suffix: `w-f · ${f}`,
      },
      {
        key: nine,
        name: "solo",
        token: "#solo",
        disabledReason: null,
        suffix: null,
      },
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

test("complete: a post sends its own recipients, else the thread's, else nothing", () => {
  const to = { names: ["@lead · #alpha"], mentions: "#alpha @lead" };
  assert.equal(postSource("  @codex hi ", to), "@codex hi");
  assert.equal(postSource("thanks", to), "#alpha @lead thanks");
  assert.equal(postSource("#alpha hi", to), "#alpha @lead #alpha hi");
  assert.equal(postSource("hello", null), null);
  assert.equal(postSource("   ", to), null);
});
