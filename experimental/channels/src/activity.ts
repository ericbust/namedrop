import {
  activityCopy,
  overflowCopy,
  participantCopy,
  SHOWN_RECIPIENTS,
} from "./copy.ts";
import type {
  Activity,
  AddressId,
  Attention,
  Channel,
  Home,
  MailItem,
  Member,
  Mention,
  PeekTarget,
  SafeText,
  SpaceKey,
} from "./model.ts";
import { type PeekKey, peekKey, peekTarget } from "./peek.ts";
import { channelSuffix, compareChannels, participant } from "./store.ts";

export type ActivityGroupKind = "needs_you" | "waiting" | "working";
const GROUP_ORDER: readonly ActivityGroupKind[] = [
  "needs_you",
  "waiting",
  "working",
];
const GROUP_OF = {
  blocked: "needs_you",
  waiting: "waiting",
  working: "working",
  retired: null,
  offline: null,
  idle: null,
  unknown: null,
} as const satisfies Record<Activity, ActivityGroupKind | null>;

export type ActivityLine =
  | { kind: "attention"; summary: SafeText }
  | { kind: "waiting"; on: readonly Mention[] }
  | { kind: "mail"; item: MailItem; held: boolean }
  | { kind: "nothing" };
export type ActivityRow = Readonly<{
  key: PeekKey;
  group: ActivityGroupKind;
  member: Member;
  peek: PeekTarget;
  channel: Readonly<{
    key: SpaceKey;
    displayName: string;
    workspaceId: string;
    suffix: string | null;
  }>;
  line: ActivityLine;
  since: number | null;
}>;
export type ActivityGroup = Readonly<{
  kind: ActivityGroupKind;
  rows: readonly ActivityRow[];
}>;
export type ActivityList = Readonly<{
  groups: readonly ActivityGroup[];
  count: number;
  byKey: ReadonlyMap<PeekKey, ActivityRow>;
}>;

function newestAbout(
  attention: readonly Attention[],
  alias: string,
): Attention | undefined {
  let newest: Attention | undefined;
  for (const item of attention)
    if (
      item.subjectAlias === alias &&
      (newest === undefined || item.openedAt > newest.openedAt)
    )
      newest = item;
  return newest;
}
function lineFor(
  channel: Channel,
  member: Member,
  group: ActivityGroupKind,
): ActivityLine {
  const about =
    group === "needs_you"
      ? newestAbout(channel.attention, member.alias)
      : undefined;
  if (about !== undefined) return { kind: "attention", summary: about.summary };
  const work = channel.work.get(member.id);
  if (work !== undefined && work.waitingOn.length > 0)
    return { kind: "waiting", on: work.waitingOn };
  const oldest = work?.mailbox[0];
  if (oldest === undefined) return { kind: "nothing" };
  const paused =
    channel.health.level === "paused" &&
    (oldest.state === "queued" || oldest.state === "dispatching");
  return {
    kind: "mail",
    item: oldest,
    held: oldest.state === "held" || paused,
  };
}
function sameLine(a: ActivityLine, b: ActivityLine): boolean {
  switch (a.kind) {
    case "attention":
      return b.kind === "attention" && a.summary === b.summary;
    case "waiting":
      return b.kind === "waiting" && a.on === b.on;
    case "mail":
      return b.kind === "mail" && a.item === b.item && a.held === b.held;
    case "nothing":
      return b.kind === "nothing";
    default: {
      const exhaustive: never = a;
      return exhaustive;
    }
  }
}
function sameRow(a: ActivityRow, b: ActivityRow): boolean {
  return (
    a.member === b.member &&
    a.group === b.group &&
    a.since === b.since &&
    a.channel.displayName === b.channel.displayName &&
    a.channel.workspaceId === b.channel.workspaceId &&
    a.channel.suffix === b.channel.suffix &&
    sameLine(a.line, b.line)
  );
}
const collator = new Intl.Collator();
function compareRows(a: ActivityRow, b: ActivityRow): number {
  if (a.since !== b.since) {
    if (a.since === null) return 1;
    if (b.since === null) return -1;
    return a.since - b.since;
  }
  return (
    compareChannels(a.channel, b.channel) ||
    collator.compare(a.member.alias, b.member.alias) ||
    collator.compare(a.member.id, b.member.id)
  );
}
function sameRows(a: readonly ActivityRow[], b: readonly ActivityRow[]) {
  return a.length === b.length && a.every((row, index) => row === b[index]);
}

// A rebuilt row equal to the previous one is replaced by it, so rows keep their
// identity across frames and a frame that changes nothing returns `previous`.
export function activityOf(
  channels: ReadonlyMap<SpaceKey, Channel>,
  previous?: ActivityList,
): ActivityList {
  const buckets: Record<ActivityGroupKind, ActivityRow[]> = {
    needs_you: [],
    waiting: [],
    working: [],
  };
  const byKey = new Map<PeekKey, ActivityRow>();
  for (const channel of channels.values()) {
    let suffix: string | null | undefined;
    for (const member of channel.members.values()) {
      const group = GROUP_OF[member.activity];
      const peek = group === null ? null : peekTarget(channel.key, member);
      if (group === null || peek === null) continue;
      suffix ??= channelSuffix(channel, channels.values());
      const built: ActivityRow = {
        key: peekKey(peek),
        group,
        member,
        peek,
        channel: {
          key: channel.key,
          displayName: channel.displayName,
          workspaceId: channel.workspaceId,
          suffix,
        },
        line: lineFor(channel, member, group),
        since: channel.work.get(member.id)?.mailbox[0]?.since ?? null,
      };
      const old = previous?.byKey.get(built.key);
      const row = old !== undefined && sameRow(old, built) ? old : built;
      buckets[group].push(row);
      byKey.set(row.key, row);
    }
  }
  const groups = GROUP_ORDER.flatMap((kind): ActivityGroup[] => {
    const rows = buckets[kind].sort(compareRows);
    if (rows.length === 0) return [];
    const old = previous?.groups.find((group) => group.kind === kind);
    return [
      old !== undefined && sameRows(old.rows, rows) ? old : { kind, rows },
    ];
  });
  return previous !== undefined &&
    previous.groups.length === groups.length &&
    previous.groups.every((group, index) => group === groups[index])
    ? previous
    : { groups, count: byKey.size, byKey };
}

export function activitySummary(list: ActivityList): string {
  return list.groups
    .map((group) => `${group.rows.length} ${activityCopy.counted[group.kind]}`)
    .join(" · ");
}

const noMembers: ReadonlyMap<AddressId, Member> = new Map();
function mentionText(mention: Mention, homes: ReadonlyMap<string, Home>) {
  switch (mention.kind) {
    case "operator":
      return "you";
    case "alias":
      return mention.alias;
    case "envoy": {
      const copy = participantCopy(
        participant({ members: noMembers, homes }, mention.id),
      );
      return copy.suffix === null ? copy.name : `${copy.name} · ${copy.suffix}`;
    }
    default: {
      const exhaustive: never = mention;
      return exhaustive;
    }
  }
}
function shortened({ text, truncated }: SafeText): string {
  return truncated ? `${text}…` : text;
}
// Names resolve at render, so a change to any workspace's home never rebuilds rows.
export function lineText(
  line: ActivityLine,
  homes: ReadonlyMap<string, Home>,
): string {
  switch (line.kind) {
    case "attention":
      return shortened(line.summary);
    case "waiting": {
      const names = line.on.map((mention) => mentionText(mention, homes));
      const hidden = names.length - SHOWN_RECIPIENTS;
      return `Waiting on ${names.slice(0, SHOWN_RECIPIENTS).join(", ")}${hidden > 0 ? ` ${overflowCopy(hidden)}` : ""}`;
    }
    case "mail": {
      const { from, preview } = line.item;
      return `From ${mentionText(from, homes)}: “${shortened(preview)}”`;
    }
    case "nothing":
      return "";
    default: {
      const exhaustive: never = line;
      return exhaustive;
    }
  }
}
