import { composerCopy } from "./copy.ts";
import { foldLabel } from "./label.ts";
import type {
  AddressId,
  ChannelSnapshot,
  Member,
  Outgoing,
  SendRule,
  SpaceKey,
} from "./model.ts";
import { channelSuffix } from "./store.ts";

export type DisabledReason =
  | "unusable-label"
  | "no-label"
  | "shared-name"
  | "unknown";
export type ChannelOption = {
  key: SpaceKey;
  name: string;
  suffix: string | null;
  token: string | null;
  disabledReason: DisabledReason | null;
};
export type CompletionContext = {
  current: SpaceKey;
  channels: readonly ChannelOption[];
  membersOf(key: SpaceKey): readonly Member[];
};
export type Token = {
  kind: "channel" | "member";
  start: number;
  end: number;
  query: string;
};
export type Candidate =
  | { kind: "channel"; channel: ChannelOption }
  | { kind: "all" }
  | { kind: "member"; member: Member };
export type Completion = {
  token: Token;
  target: SpaceKey;
  candidates: readonly Candidate[];
};

const ALL = "#all";
const WHITESPACE = /\s/;

function disabledReason(
  reason: ChannelSnapshot["notAddressableReason"],
): DisabledReason {
  switch (reason) {
    case "empty_label":
      return "unusable-label";
    case "missing_label":
      return "no-label";
    case "shared_label":
      return "shared-name";
    case null:
    case "reserved_all":
    case "id_collision":
    case "not_activated":
    case "not_active":
    case "unavailable":
      return "unknown";
    default: {
      const exhaustive: never = reason;
      return exhaustive;
    }
  }
}

export function channelOptions(
  channels: Iterable<
    Pick<
      ChannelSnapshot,
      | "key"
      | "displayName"
      | "addressableLabel"
      | "notAddressableReason"
      | "workspaceId"
    >
  >,
): ChannelOption[] {
  const all = [...channels];
  return all.map((channel) => {
    const suffix = channelSuffix(channel, all);
    return {
      key: channel.key,
      name: channel.displayName,
      suffix,
      token:
        channel.addressableLabel === null
          ? null
          : `#${channel.addressableLabel}`,
      disabledReason:
        channel.addressableLabel === null
          ? disabledReason(channel.notAddressableReason)
          : null,
    };
  });
}

function tokenAt(text: string, caret: number): Token | null {
  let start = caret;
  while (start > 0 && !WHITESPACE.test(text.charAt(start - 1))) start--;
  if (start === caret) return null;
  let end = caret;
  while (end < text.length && !WHITESPACE.test(text.charAt(end))) end++;
  const sigil = text.charAt(start);
  if (sigil !== "#" && sigil !== "@") return null;
  return {
    kind: sigil === "#" ? "channel" : "member",
    start,
    end,
    query: text.slice(start + 1, end),
  };
}

// An item matches when any of its names contains the query. The first name sorts ties.
function ranked<T>(
  items: readonly T[],
  names: (item: T) => readonly [string, ...string[]],
  query: string,
): T[] {
  const needle = query.toLowerCase();
  return items
    .flatMap((item) => {
      const [first, ...rest] = names(item);
      const name = first.toLowerCase();
      const found = [name, ...rest.map((text) => text.toLowerCase())]
        .map((text) => text.indexOf(needle))
        .filter((at) => at >= 0);
      return found.length === 0
        ? []
        : [{ item, prefix: found.includes(0), name }];
    })
    .sort(
      (a, b) =>
        Number(b.prefix) - Number(a.prefix) ||
        (a.name < b.name ? -1 : a.name > b.name ? 1 : 0),
    )
    .map(({ item }) => item);
}

function bareAlias(member: Member): string {
  return member.alias.replace(/^@+/, "");
}

function bareToken(channel: ChannelOption): string | null {
  return channel.token === null ? null : channel.token.slice(1);
}

function targetBefore(
  text: string,
  end: number,
  context: CompletionContext,
): SpaceKey {
  const byToken = new Map(
    context.channels.flatMap((channel) => {
      const token = bareToken(channel);
      return token === null ? [] : [[token, channel.key] as const];
    }),
  );
  let target = context.current;
  for (const match of text.slice(0, end).matchAll(/(?:^|\s)#(\S+)/g)) {
    const key = byToken.get(foldLabel(match[1] ?? ""));
    if (key !== undefined) target = key;
  }
  return target;
}

export function complete(
  text: string,
  caret: number,
  context: CompletionContext,
): Completion | null {
  const token = tokenAt(text, caret);
  if (token === null) return null;
  if (token.kind === "channel") {
    const candidates: Candidate[] = ranked(
      context.channels,
      (channel) => {
        const bare = bareToken(channel);
        return bare === null ? [channel.name] : [channel.name, bare];
      },
      token.query,
    ).map((channel) => ({ kind: "channel", channel }));
    if (ALL.slice(1).includes(token.query.toLowerCase()))
      candidates.push({ kind: "all" });
    return { token, target: context.current, candidates };
  }
  const target = targetBefore(text, token.start, context);
  const members = context
    .membersOf(target)
    .filter(
      (member) => member.kind.type !== "human" && member.activity !== "retired",
    );
  return {
    token,
    target,
    candidates: ranked(
      members,
      (member) => [bareAlias(member)],
      token.query,
    ).map((member) => ({ kind: "member", member })),
  };
}

export function insertion(candidate: Candidate): string | null {
  switch (candidate.kind) {
    case "channel":
      return candidate.channel.token;
    case "all":
      return ALL;
    case "member":
      return `@${bareAlias(candidate.member)}`;
    default: {
      const exhaustive: never = candidate;
      return exhaustive;
    }
  }
}

export function accept(
  text: string,
  completion: Completion,
  candidate: Candidate,
): { text: string; caret: number } | null {
  const value = insertion(candidate);
  if (value === null) return null;
  const { start, end } = completion.token;
  const rest = text.slice(end);
  return {
    text: `${text.slice(0, start)}${value}${rest.startsWith(" ") ? "" : " "}${rest}`,
    caret: start + value.length + 1,
  };
}

// The server's send grammar: any `#label` tokens, then at least one valid `@alias`.
const LEADING = /^\s*(?:#[\w.-]+\s+)*((?:@[a-z0-9]+(?:-[a-z0-9]+)*(?:\s+|$))+)/;

// The aliases a post starts with, as typed, and the message after them.
function leadingRun(text: string): {
  aliases: readonly string[];
  rest: string;
} {
  const match = LEADING.exec(text);
  const run = match?.[1];
  return match === null || run === undefined
    ? { aliases: [], rest: text }
    : { aliases: run.trim().split(/\s+/), rest: text.slice(match[0].length) };
}
// A post that starts with aliases goes only to them.
export function leadingMentions(text: string): readonly string[] {
  return leadingRun(text).aliases;
}

// A mention anywhere: not inside a word or an email address, not escaped, and not the start of a longer alias.
const MENTION = /(?<![\p{L}\p{N}_\\])@[a-z0-9]+(?:-[a-z0-9]+)*(?![a-z0-9-])/gu;

// The agents a channel post mentions, in the order the text first names them.
export function mentionedAgents(
  text: string,
  members: ReadonlyMap<AddressId, Member>,
): readonly Member[] {
  const agents = new Map<string, Member>();
  for (const member of members.values())
    if (member.kind.type === "agent" && member.activity !== "retired")
      agents.set(member.alias, member);
  const mentioned = new Set<Member>();
  for (const [alias] of text.matchAll(MENTION)) {
    const member = agents.get(alias);
    if (member !== undefined) mentioned.add(member);
  }
  return [...mentioned];
}

// The text Enter sends, or null when it must stay in the box. The server
// refuses names with no message after them.
export function postSource(text: string, rule: SendRule): string | null {
  const body = text.trim();
  const { aliases, rest } = leadingRun(body);
  if (aliases.length > 0) return rest.trim() === "" ? null : body;
  return body === "" || (rule.kind === "thread" && rule.to.length === 0)
    ? null
    : body;
}

// Where Enter sends the text, in the words of the hint under the box.
export function composerHint(rule: SendRule, text: string): string {
  if (rule.kind === "thread") return composerCopy.hint;
  const leading = leadingMentions(text);
  if (leading.length > 0) return composerCopy.sendsOnlyTo(leading);
  const mentioned = mentionedAgents(text, rule.agents);
  return mentioned.length > 0
    ? composerCopy.checks(mentioned.map((member) => member.alias))
    : composerCopy.toRoom;
}

export type PendingStage = "sending" | "checking" | "sent";
// A channel post that mentions agents waits on the router before the server answers.
export function pendingStage(
  entry: Pick<Outgoing, "composer" | "source" | "phase">,
  members: ReadonlyMap<AddressId, Member>,
): PendingStage {
  if (entry.phase.kind === "sent") return "sent";
  return entry.composer.thread === null &&
    leadingMentions(entry.source).length === 0 &&
    mentionedAgents(entry.source, members).length > 0
    ? "checking"
    : "sending";
}
