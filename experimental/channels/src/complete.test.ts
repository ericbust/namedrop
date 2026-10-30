import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accept,
  type Candidate,
  type Completion,
  type CompletionContext,
  channelOptions,
  complete,
  composerHint,
  leadingMentions,
  mentionedAgents,
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
  reason: string | null = null,
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
          not_addressable_reason: reason,
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
  snapshot("d", "w-d", null, [], "w-d", "active", "missing_label"),
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
  const cases = [
    ["missing_label", "no-label"],
    ["empty_label", "unusable-label"],
    ["shared_label", "shared-name"],
    ["reserved_all", "unknown"],
    ["id_collision", "unknown"],
    ["not_activated", "unknown"],
    ["not_active", "unknown"],
    ["unavailable", "unknown"],
  ];
  for (const [reason, expected] of cases) {
    assert.ok(reason && expected);
    const channel = snapshot(
      "1",
      "any display",
      null,
      [],
      "w1",
      "active",
      reason,
    );
    assert.equal(channelOptions([channel])[0]?.disabledReason, expected);
  }
});

test("complete: old frames without a reason use the existing general copy", () => {
  assert.deepEqual(
    channelOptions([
      snapshot("1", "same", null),
      snapshot("2", "same", null),
    ]).map((option) => option.disabledReason),
    ["unknown", "unknown"],
  );
});
test("complete: channel options that share a name carry the sidebar's workspace suffix", () => {
  const e = "e".repeat(20);
  const f = "f".repeat(20);
  const nine = "9".repeat(20);
  assert.deepEqual(
    channelOptions([
      snapshot("e", "twin", null, [], "w-e", "active", "shared_label"),
      snapshot("f", "twin", null, [], "w-f", "active", "shared_label"),
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
  const cases: [string, string[]][] = [
    ["@codex hi", ["@codex"]],
    ["  #beta @codex hi", ["@codex"]],
    ["#all @codex hi", ["@codex"]],
    ["#alpha #beta @lead @reviewer hi", ["@lead", "@reviewer"]],
    [
      "@codex  @shell\n@claude-2 run it @lead",
      ["@codex", "@shell", "@claude-2"],
    ],
    ["@codex @Shell hi", ["@codex"]],
    ["@codex\nhi", ["@codex"]],
    ["@codex", ["@codex"]],
    ["hello", []],
    ["hi @codex", []],
    ["@ alone", []],
    ["#alpha hi", []],
    ["#alpha", []],
    ["#42 fix the build", []],
    ["@Codex hi", []],
    ["@codex, hi", []],
    ["@-codex hi", []],
    ["@co--dex hi", []],
    ["#alpha hi @codex", []],
  ];
  for (const [text, named] of cases)
    assert.deepEqual(leadingMentions(text), named, JSON.stringify(text));
});

test("complete: a channel post mentions the agents it names anywhere, once each, in the order it names them", () => {
  const { members } = alpha;
  const named = (text: string) =>
    mentionedAgents(text, members).map((member) => member.alias);
  assert.deepEqual(named("can @claude ask @codex, then @claude again?"), [
    "@claude",
    "@codex",
  ]);
  assert.deepEqual(named("@codex: look"), ["@codex"]);
  assert.deepEqual(named("(@codex) and @claude."), ["@codex", "@claude"]);
  assert.deepEqual(named("mail me at ops@codex or x_@codex or 1@codex"), []);
  assert.deepEqual(named("escaped \\@codex stays text"), []);
  assert.deepEqual(named("@codex-2 and @codex- and @codexx"), []);
  assert.deepEqual(named("@Codex and @CODEX"), []);
  assert.deepEqual(named("@shell ls"), [], "a terminal never counts");
  assert.deepEqual(named("@ghost come back"), [], "a retired agent");
  assert.deepEqual(named("@you and @unknown"), [], "not agents");
  assert.deepEqual(named("é@codex and ü @codex"), ["@codex"]);
});

test("complete: the hint finds mentions the way the server does, in code spans, after other characters, and in whole tokens", () => {
  const { members } = alpha;
  const named = (text: string) =>
    mentionedAgents(text, members).map((member) => member.alias);
  const cases: [string, string[]][] = [
    ["can @claude ask @codex, then @claude again?", ["@claude", "@codex"]],
    ["`@codex` stays code", []],
    ["``say `@codex` here`` and @claude", ["@claude"]],
    ["`` a ` @codex `` then @claude", ["@claude"]],
    ["```@codex``` @claude", ["@claude"]],
    ["unclosed `code @codex and @claude", []],
    ["unclosed ``code` @codex", []],
    ["x@codex 1@codex _@codex \\@codex X@claude", []],
    ["é@claude", ["@claude"]],
    ["日本@codex", ["@codex"]],
    ["-@codex (@claude)", ["@codex", "@claude"]],
    ["@@codex", ["@codex"]],
    ["@codex@claude", ["@codex"]],
    ["@codex_x and @codex-x and @co--dex and @codex-", []],
    ["@Codex and @CODEX", []],
    ["@codex. @claude,", ["@codex", "@claude"]],
    ["@codex: look", ["@codex"]],
    ["mail me at ops@codex", []],
    ["@shell ls and @ghost and @you", []],
  ];
  for (const [text, expected] of cases)
    assert.deepEqual(named(text), expected, JSON.stringify(text));
  const leading: [string, string[]][] = [
    ["@codex @codex hi", ["@codex"]],
    ["#ALL @codex hi", ["@codex"]],
    ["#al!pha @codex hi", []],
    ["@codex_x hi", []],
    ["@codex. hi", []],
    ["`@codex` hi", []],
    ["\t#alpha\n@codex\u00a0@claude hi", ["@codex", "@claude"]],
    ["@codex\u0085hi", ["@codex"]],
    ["@codex\ufeffhi", []],
    ["@codex\u3000hi", ["@codex"]],
  ];
  for (const [text, expected] of leading)
    assert.deepEqual(leadingMentions(text), expected, JSON.stringify(text));
  const channel = { kind: "channel", agents: members } as const;
  assert.equal(
    composerHint(channel, "ask `@codex` about it"),
    "Enter posts to the channel · Mention @name to bring someone in",
  );
  assert.equal(
    composerHint(channel, "é@claude look"),
    "Enter posts to the channel · Namedrop checks if it's for @claude",
  );
  assert.equal(
    composerHint(channel, "@codex @codex hi"),
    "Enter sends only to @codex",
  );
});

test("complete: a channel post sends as typed, a thread reply needs agents or its own mentions", () => {
  const channel = { kind: "channel", agents: alpha.members } as const;
  assert.equal(postSource("  hello room ", channel), "hello room");
  assert.equal(postSource("can @codex look?", channel), "can @codex look?");
  assert.equal(postSource("@codex hi", channel), "@codex hi");
  assert.equal(postSource("  \n ", channel), null);
  const agents = { kind: "thread", to: ["@lead · #alpha"] } as const;
  assert.equal(postSource(" thanks ", agents), "thanks", "never prefixed");
  assert.equal(postSource("#alpha hi", agents), "#alpha hi");
  assert.equal(postSource("@codex hi", agents), "@codex hi");
  const nobody = { kind: "thread", to: [] } as const;
  assert.equal(postSource("ls -la", nobody), null);
  assert.equal(postSource("hi @shell", nobody), null);
  assert.equal(postSource("@shell ls -la", nobody), "@shell ls -la");
  assert.equal(postSource("#alpha @shell ls", nobody), "#alpha @shell ls");
  assert.equal(postSource("   ", agents), null);
  for (const rule of [channel, agents, nobody])
    for (const text of ["@codex", " @codex  ", "#alpha @codex @shell\n"])
      assert.equal(
        postSource(text, rule),
        null,
        `${JSON.stringify(text)} names someone but has no message`,
      );
});

test("complete: the hint says where Enter sends the text", () => {
  const channel = { kind: "channel", agents: alpha.members } as const;
  const hint = (text: string) => composerHint(channel, text);
  const room = "Enter posts to the channel · Mention @name to bring someone in";
  assert.equal(hint(""), room);
  assert.equal(hint("heads up, keys rotate at 3pm"), room);
  assert.equal(hint("ask @shell and @ghost"), room, "no agent mentioned");
  assert.equal(
    hint("can @codex look?"),
    "Enter posts to the channel · Namedrop checks if it's for @codex",
  );
  assert.equal(
    hint("@ @claude, @codex and @claude"),
    "Enter posts to the channel · Namedrop checks if it's for @claude, @codex",
  );
  assert.equal(hint("@codex hi"), "Enter sends only to @codex");
  assert.equal(
    hint("#alpha @codex @shell hi"),
    "Enter sends only to @codex, @shell",
  );
  assert.equal(
    hint("@codex @shell @lead @claude hi"),
    "Enter sends only to @codex, @shell +2",
  );
  for (const to of [[], ["@codex"]])
    assert.equal(
      composerHint({ kind: "thread", to }, "@codex hi"),
      "Enter to send · Shift+Enter for a new line",
    );
});

test("complete: disabled copy follows the server reason despite sanitized or identical names", () => {
  const channels = [
    snapshot(
      "1",
      "same name",
      null,
      [],
      "w-1",
      "not_activated",
      "shared_label",
    ),
    snapshot(
      "2",
      "same name",
      null,
      [],
      "w-2",
      "not_activated",
      "shared_label",
    ),
    snapshot("3", "all", null, [], "w-3", "active", "reserved_all"),
    snapshot("4", "all", null, [], "w-4", "active", "reserved_all"),
    snapshot("5", "w-5", null, [], "w-5", "active", "id_collision"),
    snapshot("6", "日本語", null, [], "w-6", "active", "empty_label"),
    snapshot("7", "anything", null, [], "w-7", "active", "missing_label"),
  ];
  assert.deepEqual(
    channelOptions(channels).map((option) => option.disabledReason),
    [
      "shared-name",
      "shared-name",
      "unknown",
      "unknown",
      "unknown",
      "unusable-label",
      "no-label",
    ],
  );
});
