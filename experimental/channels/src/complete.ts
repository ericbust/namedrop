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
// A name typed with punctuation stuck to it, as in "@codex;" or "@codex,,".
const PUNCTUATED = /^@([A-Za-z0-9-]+)[,.:;!?]+$/;
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
// A leading `#label` or `@name` word may end in one `,` or `:`, which is not part of it.
function recipient(word: string): string {
  return /[,:]$/.test(word) ? word.slice(0, -1) : word;
}
function afterLabels(text: string): number {
  let at = spaceAfter(text, 0);
  while (LABEL.test(recipient(wordAt(text, at))))
    at = spaceAfter(text, at + wordAt(text, at).length);
  return at;
}
// The server's workspace_label_before: an `@` after whitespace that follows a
// `#label` word, `#all` aside, names someone in that workspace.
function afterLabelWord(text: string, at: number): boolean {
  let end = at;
  while (end > 0 && WHITESPACE.test(text.charAt(end - 1))) end--;
  if (end === at) return false;
  let start = end;
  while (start > 0 && !WHITESPACE.test(text.charAt(start - 1))) start--;
  const word = text.slice(start, end);
  return LABEL.test(word) && foldLabel(word.slice(1)) !== "all";
}
function serverMentions(text: string): Mentions {
  let at = afterLabels(text);
  const leading = new Set<string>();
  let word = wordAt(text, at);
  while (word.startsWith("@") && ALIAS.test(recipient(word).slice(1))) {
    leading.add(recipient(word));
    at = spaceAfter(text, at + word.length);
    word = wordAt(text, at);
  }
  // Channel mentions, as the server's context_mention_spans counts them. A name
  // after a `#label` word, and each name after it with only whitespace between,
  // belongs to that workspace, so it is not one.
  const anywhere = new Set<string>();
  let qualifiedEnd: number | null = null;
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
      const qualified: boolean =
        (qualifiedEnd !== null && spaceAfter(text, qualifiedEnd) === cursor) ||
        afterLabelWord(text, cursor);
      qualifiedEnd = qualified ? end : null;
      const alias = text.slice(cursor + 1, end);
      if (!qualified && ALIAS.test(alias)) anywhere.add(`@${alias}`);
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
// The names whose panes are closed, unless a current member took the name.
function closedNames(members: ReadonlyMap<AddressId, Member>): Set<string> {
  const closed = new Set<string>();
  const current = new Set<string>();
  for (const member of members.values())
    (member.activity === "retired" ? closed : current).add(member.alias);
  for (const alias of current) closed.delete(alias);
  return closed;
}
// The hint for leading text the server refuses, read in its order: after any
// `#label` words, the first `@` word must be a whole name, and no leading name
// may have capitals. The server refuses a capital before it looks the name up,
// so that is the reason whether or not the page knows the name. A lowercase
// name the page doesn't know may be a group, which the frame doesn't carry, so
// the server decides that one. Without a `#label`, a name whose pane is closed
// comes before any capital: lowercasing it would only meet a second refusal.
function refusedLeading(
  text: string,
  members: ReadonlyMap<AddressId, Member>,
): string | null {
  let at = afterLabels(text);
  const labelled = at > spaceAfter(text, 0);
  let word = wordAt(text, at);
  const isName = (token: string) =>
    token.startsWith("@") && ALIAS.test(asciiLower(recipient(token).slice(1)));
  if (word.length > 1 && word.startsWith("@") && !isName(word)) {
    // Text that starts with a #label is never a leading mention to the server,
    // so it posts to the channel as it is.
    if (labelled) return null;
    const name = PUNCTUATED.exec(word)?.[1];
    return name !== undefined && ALIAS.test(asciiLower(name))
      ? composerCopy.spaceAfter(`@${asciiLower(name)}`)
      : composerCopy.notMember(word);
  }
  const names: string[] = [];
  while (isName(word)) {
    names.push(recipient(word));
    at = spaceAfter(text, at + word.length);
    word = wordAt(text, at);
  }
  const closed = labelled ? new Set<string>() : closedNames(members);
  const closedName = names.map(asciiLower).find((name) => closed.has(name));
  if (closedName !== undefined) return composerCopy.paneClosed(closedName);
  const capital = names.find((name) => name !== asciiLower(name));
  return capital === undefined
    ? null
    : composerCopy.lowercase(asciiLower(capital));
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

// The text Enter sends. The hint judges this same string.
export function sentText(text: string): string {
  return text.trim();
}
// The server reads text that is only `@` as a leading mention with no name.
function loneAt(sent: string): boolean {
  return sent.slice(spaceAfter(sent, 0)) === "@";
}

// The text Enter sends, or null when it must stay in the box.
export function postSource(text: string, rule: SendRule): string | null {
  const body = sentText(text);
  if (loneAt(body)) return null;
  const { leading, rest } = serverMentions(body);
  if (leading.length > 0) return rest.trim() === "" ? null : body;
  return body === "" || (rule.kind === "thread" && rule.to.length === 0)
    ? null
    : body;
}

// Why Enter kept the text in the box, for the line under it.
export function keptHint(text: string): string {
  const body = sentText(text);
  if (loneAt(body)) return composerCopy.nameAfter;
  const leading = leadingMentions(body);
  return leading.length > 0
    ? composerCopy.needsMessage(leading)
    : composerCopy.needsRecipient;
}

// Where Enter sends the text, in the words of the hint under the box.
export function composerHint(rule: SendRule, text: string): string {
  const body = sentText(text);
  if (loneAt(body)) return composerCopy.nameAfter;
  if (rule.kind === "thread") return composerCopy.hint;
  const refused = refusedLeading(body, rule.agents);
  if (refused !== null) return refused;
  const leading = leadingMentions(body);
  if (leading.length > 0) return composerCopy.sendsOnlyTo(leading);
  const mentioned = mentionedAgents(body, rule.agents);
  return mentioned.length > 0
    ? composerCopy.checks(mentioned.map((member) => member.alias))
    : composerCopy.toRoom;
}
