import type { Channel, Event, SpaceKey, State } from "./model.ts";

export function initialState(visible = true): State {
  return {
    channels: new Map(),
    selectedKey: null,
    unread: new Set(),
    sync: { kind: "connecting" },
    visible,
  };
}
function sameMembers(a: Channel, b: Channel): boolean {
  return JSON.stringify([...a.members]) === JSON.stringify([...b.members]);
}
function sameMessages(a: Channel, b: Channel): boolean {
  return (
    a.messages.size === b.messages.size &&
    [...a.messages.keys()].every((key) => b.messages.has(key))
  );
}
function sameAttention(a: Channel, b: Channel): boolean {
  return JSON.stringify(a.attention) === JSON.stringify(b.attention);
}
function sameChannel(a: Channel, b: Channel): boolean {
  return (
    a.workspaceId === b.workspaceId &&
    a.displayName === b.displayName &&
    a.addressableLabel === b.addressableLabel &&
    JSON.stringify(a.health) === JSON.stringify(b.health) &&
    sameMembers(a, b) &&
    sameMessages(a, b) &&
    sameAttention(a, b)
  );
}
function hasActivity(a: Channel, b: Channel): boolean {
  return (
    [...b.messages.keys()].some((id) => !a.messages.has(id)) ||
    !sameAttention(a, b) ||
    a.members.size !== b.members.size ||
    [...a.members].some(
      ([id, member]) => b.members.get(id)?.activity !== member.activity,
    )
  );
}
function clearUnread(
  unread: ReadonlySet<SpaceKey>,
  key: SpaceKey | null,
): ReadonlySet<SpaceKey> {
  if (key === null || !unread.has(key)) return unread;
  const next = new Set(unread);
  next.delete(key);
  return next;
}
function finishReplay(state: State, keys: ReadonlySet<SpaceKey> | null): State {
  if (state.sync.kind !== "replaying") return state;
  const channels = new Map(state.channels);
  for (const [key, channel] of state.sync.pending) {
    const previous = state.channels.get(key);
    channels.set(
      key,
      previous && sameChannel(previous, channel) ? previous : channel,
    );
  }
  if (keys !== null) {
    for (const key of channels.keys()) {
      if (!keys.has(key)) channels.delete(key);
    }
  }
  const selectedKey =
    state.selectedKey !== null && channels.has(state.selectedKey)
      ? state.selectedKey
      : (sortedChannels({ ...state, channels })[0]?.key ?? null);
  const unread = new Set([...state.unread].filter((key) => channels.has(key)));
  for (const [key, channel] of channels) {
    const previous = state.channels.get(key);
    if (previous && hasActivity(previous, channel)) unread.add(key);
  }
  if (state.visible && selectedKey !== null) unread.delete(selectedKey);
  return { ...state, channels, selectedKey, unread, sync: { kind: "live" } };
}
export function reduce(state: State, event: Event): State {
  switch (event.type) {
    case "space": {
      const { channel } = event;
      if (state.sync.kind === "replaying") {
        const pending = new Map(state.sync.pending);
        pending.set(channel.key, channel);
        return { ...state, sync: { ...state.sync, pending } };
      }
      const previous = state.channels.get(channel.key);
      if (previous && sameChannel(previous, channel)) return state;
      const channels = new Map(state.channels);
      channels.set(channel.key, channel);
      const selectedKey =
        state.channels.size === 0 ? channel.key : state.selectedKey;
      let unread = state.unread;
      if (
        previous &&
        hasActivity(previous, channel) &&
        !(selectedKey === channel.key && state.visible)
      )
        unread = new Set([...unread, channel.key]);
      return { ...state, channels, selectedKey, unread };
    }
    case "ready":
      return finishReplay(state, event.keys);
    case "replay-timeout":
      return finishReplay(state, null);
    case "gone": {
      if (
        !state.channels.has(event.key) &&
        !(state.sync.kind === "replaying" && state.sync.pending.has(event.key))
      )
        return state;
      const channels = new Map(state.channels);
      channels.delete(event.key);
      let sync = state.sync;
      if (sync.kind === "replaying") {
        const pending = new Map(sync.pending);
        pending.delete(event.key);
        sync = { ...sync, pending };
      }
      return {
        ...state,
        channels,
        sync,
        selectedKey: state.selectedKey === event.key ? null : state.selectedKey,
        unread: clearUnread(state.unread, event.key),
      };
    }
    case "select": {
      if (!state.channels.has(event.key)) return state;
      const unread = state.visible
        ? clearUnread(state.unread, event.key)
        : state.unread;
      if (state.selectedKey === event.key && unread === state.unread)
        return state;
      return { ...state, selectedKey: event.key, unread };
    }
    case "opened":
      return {
        ...state,
        sync: {
          kind: "replaying",
          status:
            connectionStatus(state) === "connecting"
              ? "connecting"
              : "reconnecting",
          pending: new Map(),
        },
      };
    case "disconnected":
      return state.sync.kind === "reconnecting"
        ? state
        : { ...state, sync: { kind: "reconnecting" } };
    case "visibility": {
      const unread = event.visible
        ? clearUnread(state.unread, state.selectedKey)
        : state.unread;
      return state.visible === event.visible && unread === state.unread
        ? state
        : { ...state, visible: event.visible, unread };
    }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
export function sortedChannels(state: State): Channel[] {
  return [...state.channels.values()].sort(
    (a, b) =>
      a.displayName.localeCompare(b.displayName) ||
      a.workspaceId.localeCompare(b.workspaceId) ||
      a.key.localeCompare(b.key),
  );
}
export function channelName(
  channel: Channel,
  channels: readonly Channel[],
): string {
  const sameName = channels.filter(
    (other) => other.displayName === channel.displayName,
  );
  if (sameName.length < 2) return channel.displayName;
  return `${channel.displayName} (${channel.workspaceId} · ${channel.key})`;
}
export function connectionStatus(
  state: State,
): "connecting" | "reconnecting" | "live" {
  return state.sync.kind === "replaying" ? state.sync.status : state.sync.kind;
}
