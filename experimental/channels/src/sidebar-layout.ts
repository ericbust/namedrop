import { sidebarCopy } from "./copy.ts";
import type { Channel, SpaceKey } from "./model.ts";
import { channelCollator } from "./store.ts";

export type GroupKey = `repo:${string}` | "other";
// The operator's arrangement of the sidebar. `channels` is one flat list, and a
// group orders its channels by their place in it, so a workspace that moves to
// another repo keeps its place there.
export type Layout = {
  groups: GroupKey[];
  channels: SpaceKey[];
  collapsed: GroupKey[];
};
export type Group = {
  key: GroupKey;
  name: string;
  channels: Channel[];
  needsYou: number;
  unread: boolean;
};

export const LAYOUT_KEY = "namedrop.channels.sidebar.v1";
export const emptyLayout: Layout = { groups: [], channels: [], collapsed: [] };

export function groupKeyOf(channel: Pick<Channel, "repo">): GroupKey {
  return channel.repo === null ? "other" : `repo:${channel.repo.id}`;
}

function bySaved<T, K>(
  items: readonly T[],
  saved: readonly K[],
  keyOf: (item: T) => K,
): T[] {
  const place = new Map(saved.map((key, index) => [key, index]));
  return items
    .map((item, index) => ({
      item,
      index,
      at: place.get(keyOf(item)) ?? Number.POSITIVE_INFINITY,
    }))
    .sort((a, b) => a.at - b.at || a.index - b.index)
    .map(({ item }) => item);
}

// `channels` arrives in sortedChannels order, which is the default order of
// channels inside a group.
export function groupChannels(
  channels: readonly Channel[],
  layout: Layout,
  unread: ReadonlySet<SpaceKey>,
): Group[] {
  const groups = new Map<GroupKey, Group>();
  for (const channel of bySaved(
    channels,
    layout.channels,
    (channel) => channel.key,
  )) {
    const key = groupKeyOf(channel);
    let group = groups.get(key);
    if (group === undefined) {
      group = {
        key,
        name: channel.repo?.name ?? sidebarCopy.other,
        channels: [],
        needsYou: 0,
        unread: false,
      };
      groups.set(key, group);
    }
    group.channels.push(channel);
    if (channel.health.level === "needs_you") group.needsYou += 1;
    if (unread.has(channel.key)) group.unread = true;
  }
  const byDefault = [...groups.values()].sort(
    (a, b) =>
      Number(a.key === "other") - Number(b.key === "other") ||
      channelCollator.compare(a.name, b.name) ||
      channelCollator.compare(a.key, b.key),
  );
  return bySaved(byDefault, layout.groups, (group) => group.key);
}

function moved<T>(items: readonly T[], item: T, toIndex: number): T[] {
  const rest = items.filter((each) => each !== item);
  const at = Math.max(0, Math.min(toIndex, rest.length));
  return [...rest.slice(0, at), item, ...rest.slice(at)];
}
// Every move saves the order the operator sees now, so a group or channel that
// is gone drops out of the saved order on the next write. A flat list is Other
// alone, and saving that would rank Other above repo groups that appear later.
function written(layout: Layout, groups: readonly Group[]): Layout {
  const keys = groups.map((group) => group.key);
  return {
    groups: keys.length === 1 && keys[0] === "other" ? [] : keys,
    channels: groups.flatMap((group) =>
      group.channels.map((channel) => channel.key),
    ),
    collapsed: layout.collapsed,
  };
}
export function moveGroup(
  layout: Layout,
  visible: readonly Group[],
  key: GroupKey,
  toIndex: number,
): Layout {
  const group = visible.find((each) => each.key === key);
  if (group === undefined) return layout;
  return written(layout, moved(visible, group, toIndex));
}
export function moveChannel(
  layout: Layout,
  visible: readonly Group[],
  key: SpaceKey,
  toIndex: number,
): Layout {
  return written(
    layout,
    visible.map((group) => {
      const channel = group.channels.find((each) => each.key === key);
      return channel === undefined
        ? group
        : { ...group, channels: moved(group.channels, channel, toIndex) };
    }),
  );
}
export function toggleCollapsed(layout: Layout, key: GroupKey): Layout {
  return {
    ...layout,
    collapsed: layout.collapsed.includes(key)
      ? layout.collapsed.filter((each) => each !== key)
      : [...layout.collapsed, key],
  };
}

function strings(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}
function groupKeys(value: unknown): GroupKey[] {
  return strings(value).filter(
    (item): item is GroupKey => item === "other" || item.startsWith("repo:"),
  );
}
export function loadLayout(storage: Pick<Storage, "getItem">): Layout {
  try {
    const raw = storage.getItem(LAYOUT_KEY);
    if (raw === null) return emptyLayout;
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null) return emptyLayout;
    const saved = value as Record<string, unknown>;
    return {
      groups: groupKeys(saved.groups),
      channels: strings(saved.channels) as SpaceKey[],
      collapsed: groupKeys(saved.collapsed),
    };
  } catch {
    return emptyLayout;
  }
}
// A full or forbidden storage only costs the arrangement on the next load.
export function saveLayout(
  storage: Pick<Storage, "setItem">,
  layout: Layout,
): void {
  try {
    storage.setItem(LAYOUT_KEY, JSON.stringify(layout));
  } catch {}
}
