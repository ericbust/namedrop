import { foldLabel } from "./label.ts";
import type { ChannelSnapshot, Member, ReplyTo, SpaceKey } from "./model.ts";
import { channelSuffix } from "./store.ts";

// Why the server gave a channel no token, in the order DESIGN.md's table checks.
export type DisabledReason =
  | "no-label"
  | "unusable-label"
  | "all"
  | "shared-label"
  | "inactive";
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
  folded: string | null,
  others: readonly (string | null)[],
): DisabledReason {
  if (folded === null) return "no-label";
  if (folded === "") return "unusable-label";
  if (folded === "all") return "all";
  if (others.includes(folded)) return "shared-label";
  return "inactive";
}

export function channelOptions(
  channels: Iterable<
    Pick<
      ChannelSnapshot,
      "key" | "displayName" | "addressableLabel" | "workspaceId"
    >
  >,
): ChannelOption[] {
  const all = [...channels];
  // The server shows the workspace id as the name only when Herdr has no label.
  const folded = all.map((channel) => ({
    channel,
    token:
      channel.displayName === channel.workspaceId
        ? null
        : foldLabel(channel.displayName),
  }));
  return folded.map(({ channel, token }) => ({
    key: channel.key,
    name: channel.displayName,
    suffix: channelSuffix(channel, all),
    token:
      channel.addressableLabel === null ? null : `#${channel.addressableLabel}`,
    disabledReason:
      channel.addressableLabel !== null
        ? null
        : disabledReason(
            token,
            folded.flatMap((other) =>
              other.channel === channel ? [] : [other.token],
            ),
          ),
  }));
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
const LEADING_MENTION = /^\s*(?:#[\w.-]+\s+)*@[a-z0-9]+(?:-[a-z0-9]+)*(?:\s|$)/;

export function leadingMention(text: string): boolean {
  return LEADING_MENTION.test(text);
}

export function postSource(text: string, to: ReplyTo | null): string | null {
  const body = text.trim();
  if (body === "") return null;
  if (leadingMention(body)) return body;
  return to === null ? null : `${to.mentions} ${body}`;
}
