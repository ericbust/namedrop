import type { ChannelSnapshot, Member, SpaceKey } from "./model.ts";

export type ChannelOption = {
  key: SpaceKey;
  name: string;
  token: string | null;
  disabledReason: "shared-label" | "no-label" | null;
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

export function channelOptions(
  channels: Iterable<
    Pick<
      ChannelSnapshot,
      "key" | "displayName" | "addressableLabel" | "workspaceId"
    >
  >,
): ChannelOption[] {
  return [...channels].map((channel) => ({
    key: channel.key,
    name: channel.displayName,
    token:
      channel.addressableLabel === null ? null : `#${channel.addressableLabel}`,
    // The server falls back to the workspace id only when Herdr has no label.
    disabledReason:
      channel.addressableLabel !== null
        ? null
        : channel.displayName === channel.workspaceId
          ? "no-label"
          : "shared-label",
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

function ranked<T>(
  items: readonly T[],
  name: (item: T) => string,
  query: string,
): T[] {
  const needle = query.toLowerCase();
  return items
    .flatMap((item) => {
      const folded = name(item).toLowerCase();
      const at = folded.indexOf(needle);
      return at < 0 ? [] : [{ item, prefix: at === 0, folded }];
    })
    .sort(
      (a, b) =>
        Number(b.prefix) - Number(a.prefix) ||
        (a.folded < b.folded ? -1 : a.folded > b.folded ? 1 : 0),
    )
    .map(({ item }) => item);
}

function bareAlias(member: Member): string {
  return member.alias.replace(/^@+/, "");
}

function targetBefore(
  text: string,
  end: number,
  context: CompletionContext,
): SpaceKey {
  const byToken = new Map(
    context.channels.flatMap((channel) =>
      channel.token === null ? [] : [[channel.token, channel.key] as const],
    ),
  );
  let target = context.current;
  for (const match of text.slice(0, end).matchAll(/(?:^|\s)(#\S+)/g)) {
    const key = byToken.get(match[1] ?? "");
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
      (channel) => channel.name,
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
    candidates: ranked(members, bareAlias, token.query).map((member) => ({
      kind: "member",
      member,
    })),
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

export function leadingMention(text: string): boolean {
  return /^\s*[@#]\S/.test(text);
}
