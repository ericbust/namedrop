import { composerCopy } from "./copy.ts";
import { foldLabel } from "./label.ts";
import type {
  AddressId,
  ChannelSnapshot,
  Member,
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
// Rust's char::is_whitespace, which the server's mention grammar uses; JS \s differs at U+0085 and U+FEFF.
const WHITESPACE =
  /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;

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

const ALIAS = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const LABEL = /^#[A-Za-z0-9._-]+$/;
const TOKEN_CHAR = /[A-Za-z0-9_-]/;
const EMBEDDED_AFTER = /[A-Za-z0-9_\\]/;
type Mentions = {
  leading: readonly string[];
  rest: string;
  anywhere: readonly string[];
};
function wordAt(text: string, at: number): string {
  let end = at;
  while (end < text.length && !WHITESPACE.test(text.charAt(end))) end++;
  return text.slice(at, end);
}
function spaceAfter(text: string, at: number): number {
  while (at < text.length && WHITESPACE.test(text.charAt(at))) at++;
  return at;
}
function serverMentions(text: string): Mentions {
  let at = spaceAfter(text, 0);
  while (LABEL.test(wordAt(text, at)))
    at = spaceAfter(text, at + wordAt(text, at).length);
  const leading = new Set<string>();
  let word = wordAt(text, at);
  while (word.startsWith("@") && ALIAS.test(word.slice(1))) {
    leading.add(word);
    at = spaceAfter(text, at + word.length);
    word = wordAt(text, at);
  }
  const anywhere = new Set<string>();
  let cursor = 0;
  while (cursor < text.length) {
    const char = text.charAt(cursor);
    if (char === "`") {
      const open = cursor;
      while (text.charAt(cursor) === "`") cursor++;
      const close = text.indexOf(text.slice(open, cursor), cursor);
      cursor = close < 0 ? text.length : close + cursor - open;
    } else if (
      char === "@" &&
      !(cursor > 0 && EMBEDDED_AFTER.test(text.charAt(cursor - 1)))
    ) {
      let end = cursor + 1;
      while (end < text.length && TOKEN_CHAR.test(text.charAt(end))) end++;
      const alias = text.slice(cursor + 1, end);
      if (ALIAS.test(alias)) anywhere.add(`@${alias}`);
      cursor = end;
    } else cursor++;
  }
  return {
    leading: [...leading],
    rest: leading.size === 0 ? text : text.slice(at),
    anywhere: [...anywhere],
  };
}
export function leadingMentions(text: string): readonly string[] {
  return serverMentions(text).leading;
}
// The server lowercases ASCII only, so a name with any other letter stays invalid.
function asciiLower(text: string): string {
  return text.replace(/[A-Z]+/g, (upper) => upper.toLowerCase());
}
// The hint for a leading name the server refuses, checked in its order: after
// any `#label` words, each `@name` that is an alias once lowercased, first for
// capitals, then for a name that is not a current member. Names after a label
// belong to that channel, so only their case is checked.
function refusedLeading(
  text: string,
  members: ReadonlyMap<AddressId, Member>,
): string | null {
  let at = spaceAfter(text, 0);
  const start = at;
  while (LABEL.test(wordAt(text, at)))
    at = spaceAfter(text, at + wordAt(text, at).length);
  const labeled = at > start;
  const names: string[] = [];
  let word = wordAt(text, at);
  while (word.startsWith("@") && ALIAS.test(asciiLower(word.slice(1)))) {
    names.push(word);
    at = spaceAfter(text, at + word.length);
    word = wordAt(text, at);
  }
  const current = new Set<string>();
  for (const member of members.values())
    if (member.kind.type !== "human" && member.activity !== "retired")
      current.add(member.alias);
  const capitalized = names.find((name) => name !== asciiLower(name));
  if (capitalized !== undefined) {
    const alias = asciiLower(capitalized);
    return labeled || current.has(alias)
      ? composerCopy.lowercase(alias)
      : composerCopy.notMember(capitalized);
  }
  const unknown = labeled
    ? undefined
    : names.find((name) => !current.has(name));
  return unknown === undefined ? null : composerCopy.notMember(unknown);
}

// The agents a channel post mentions, in the order the text first names them.
export function mentionedAgents(
  text: string,
  members: ReadonlyMap<AddressId, Member>,
): readonly Member[] {
  const agents = new Map<string, Member>();
  for (const member of members.values())
    if (member.kind.type === "agent" && member.activity !== "retired")
      agents.set(member.alias, member);
  return serverMentions(text).anywhere.flatMap(
    (alias) => agents.get(alias) ?? [],
  );
}

// The text Enter sends, or null when it must stay in the box.
export function postSource(text: string, rule: SendRule): string | null {
  const body = text.trim();
  const { leading, rest } = serverMentions(body);
  if (leading.length > 0) return rest.trim() === "" ? null : body;
  return body === "" || (rule.kind === "thread" && rule.to.length === 0)
    ? null
    : body;
}

// Where Enter sends the text, in the words of the hint under the box.
export function composerHint(rule: SendRule, text: string): string {
  if (rule.kind === "thread") return composerCopy.hint;
  const refused = refusedLeading(text, rule.agents);
  if (refused !== null) return refused;
  const leading = leadingMentions(text);
  if (leading.length > 0) return composerCopy.sendsOnlyTo(leading);
  const mentioned = mentionedAgents(text, rule.agents);
  return mentioned.length > 0
    ? composerCopy.checks(mentioned.map((member) => member.alias))
    : composerCopy.toRoom;
}
