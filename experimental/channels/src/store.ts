import type {
  Channel,
  ChannelSnapshot,
  Event,
  HistoryRequest,
  Message,
  SpaceKey,
  State,
} from "./model.ts";

const channelCollator = new Intl.Collator();

export function initialState(visible = true): State {
  return {
    channels: new Map(),
    selectedKey: null,
    unread: new Set(),
    sync: { kind: "connecting" },
    visible,
    viewKnown: false,
    nextRequestId: 1,
  };
}
function sameChannel(a: Channel, b: Channel): boolean {
  return (
    JSON.stringify({
      ...a,
      members: [...a.members],
      messages: [...a.messages],
      threads: [...a.threads],
    }) ===
    JSON.stringify({
      ...b,
      members: [...b.members],
      messages: [...b.messages],
      threads: [...b.threads],
    })
  );
}
function sameAttention(a: Channel, b: Channel): boolean {
  return JSON.stringify(a.attention) === JSON.stringify(b.attention);
}

// Runs are oldest first. Only a window that shares an ID proves continuity.
function mergeRuns(
  runs: Channel["runs"],
  incoming: readonly string[],
): Channel["runs"] {
  if (incoming.length === 0) return runs;
  const ids = new Set(incoming);
  const first = runs.findIndex((run) => run.some((id) => ids.has(id)));
  if (first < 0) return [...runs, incoming];
  let last = first;
  for (let i = first + 1; i < runs.length; i++)
    if (runs[i]?.some((id) => ids.has(id))) last = i;
  const joined = runs.slice(first, last + 1).flat();
  const known = new Set(joined);
  for (let i = 0; i < incoming.length; i++) {
    const id = incoming[i];
    if (id === undefined || known.has(id)) continue;
    const next = incoming
      .slice(i + 1)
      .find((candidate) => known.has(candidate));
    const at = next === undefined ? joined.length : joined.indexOf(next);
    joined.splice(at, 0, id);
    known.add(id);
  }
  return [...runs.slice(0, first), joined, ...runs.slice(last + 1)];
}
function mergeSnapshot(
  previous: Channel | undefined,
  snapshot: ChannelSnapshot,
): Channel {
  const messages = new Map(previous?.messages);
  for (const [id, message] of snapshot.messages) messages.set(id, message);
  const channel: Channel = {
    ...snapshot,
    messages,
    runs: mergeRuns(previous?.runs ?? [], [...snapshot.messages.keys()]),
    history: previous?.history ?? { kind: "idle" },
    exhausted: previous?.exhausted ?? false,
  };
  return previous && sameChannel(previous, channel) ? previous : channel;
}
function reconcile(previous: Channel | undefined, pending: Channel): Channel {
  if (!previous) return pending;
  let runs = previous.runs;
  for (const run of pending.runs) runs = mergeRuns(runs, run);
  const messages = new Map(previous.messages);
  for (const [id, row] of pending.messages) messages.set(id, row);
  const channel = {
    ...pending,
    messages,
    runs,
    history: previous.history,
    exhausted: previous.exhausted,
  };
  return sameChannel(previous, channel) ? previous : channel;
}
function requestHistory(state: State, key: SpaceKey): State {
  const channel = state.channels.get(key);
  if (!channel || channel.history.kind === "loading") return state;
  const bridge = channel.runs.length > 1;
  const before = (bridge ? channel.runs[1] : channel.runs[0])?.[0];
  if (before === undefined || (!bridge && channel.exhausted)) return state;
  const request: HistoryRequest = {
    id: state.nextRequestId,
    key,
    before,
    purpose: bridge ? "bridge" : "older",
  };
  const channels = new Map(state.channels);
  channels.set(key, { ...channel, history: { kind: "loading", request } });
  return { ...state, channels, nextRequestId: state.nextRequestId + 1 };
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
  if (state.sync.kind !== "replaying" && state.sync.kind !== "fallback")
    return state;
  const channels = new Map(state.channels);
  const pending =
    state.sync.kind === "replaying"
      ? state.sync.pending
      : new Map<SpaceKey, Channel>();
  for (const [key, channel] of pending) {
    const previous = state.channels.get(key);
    channels.set(key, reconcile(previous, channel));
  }
  if (keys !== null) {
    for (const key of channels.keys()) {
      if (!keys.has(key)) channels.delete(key);
    }
  }
  const preserveSelection =
    state.sync.kind !== "fallback" || state.sync.preserveSelection;
  const selectedKey =
    preserveSelection &&
    state.selectedKey !== null &&
    channels.has(state.selectedKey)
      ? state.selectedKey
      : (sortedChannels({ ...state, channels })[0]?.key ?? null);
  const unread = new Set([...state.unread].filter((key) => channels.has(key)));
  for (const [key, channel] of channels) {
    const previous = state.channels.get(key);
    if (previous && hasActivity(previous, channel)) unread.add(key);
  }
  if (state.visible && selectedKey !== null) unread.delete(selectedKey);
  return {
    ...state,
    channels,
    selectedKey,
    unread,
    viewKnown: state.viewKnown || channels.size > 0 || keys !== null,
    sync:
      keys === null
        ? { kind: "fallback", preserveSelection: state.viewKnown }
        : { kind: "live" },
  };
}
export function reduce(state: State, event: Event): State {
  switch (event.type) {
    case "space": {
      const { channel } = event;
      if (state.sync.kind === "replaying") {
        const pending = new Map(state.sync.pending);
        pending.set(
          channel.key,
          mergeSnapshot(
            pending.get(channel.key) ?? state.channels.get(channel.key),
            channel,
          ),
        );
        return { ...state, sync: { ...state.sync, pending } };
      }
      const previous = state.channels.get(channel.key);
      const accumulated = mergeSnapshot(previous, channel);
      if (accumulated === previous) return state;
      const channels = new Map(state.channels);
      channels.set(channel.key, accumulated);
      const selectedKey =
        state.channels.size === 0 ? channel.key : state.selectedKey;
      let unread = state.unread;
      if (
        previous &&
        hasActivity(previous, accumulated) &&
        !(selectedKey === channel.key && state.visible)
      )
        unread = new Set([...unread, channel.key]);
      return { ...state, channels, selectedKey, unread, viewKnown: true };
    }
    case "older":
      return requestHistory(state, event.key);
    case "history": {
      const channel = state.channels.get(event.request.key);
      if (
        channel?.history.kind !== "loading" ||
        channel.history.request.id !== event.request.id
      )
        return state;
      let next: Channel;
      if (event.result.kind === "error") {
        next = {
          ...channel,
          history: {
            kind: "error",
            request: event.request,
            reason: event.result.reason,
          },
        };
      } else if (
        event.result.messages.length === 0 &&
        event.request.purpose === "bridge"
      ) {
        next = {
          ...channel,
          history: {
            kind: "error",
            request: event.request,
            reason: "The gap could not be filled. Retry loading messages.",
          },
        };
      } else {
        const messages = new Map(channel.messages);
        for (const row of event.result.messages)
          if (!messages.has(row.id)) messages.set(row.id, row);
        next = {
          ...channel,
          messages,
          runs: mergeRuns(channel.runs, [
            ...event.result.messages.map((row) => row.id),
            event.request.before,
          ]),
          history: { kind: "idle" },
          exhausted:
            channel.exhausted ||
            (event.request.purpose === "older" &&
              event.result.messages.length === 0),
        };
        if (
          next.runs.length > 1 &&
          JSON.stringify(next.runs) === JSON.stringify(channel.runs)
        )
          next = {
            ...next,
            history: {
              kind: "error",
              request: event.request,
              reason: "History made no progress. Retry loading messages.",
            },
          };
      }
      const channels = new Map(state.channels);
      channels.set(event.request.key, next);
      return { ...state, channels };
    }
    case "ready":
      return finishReplay(state, event.keys);
    case "replay-timeout":
      return state.sync.kind === "replaying"
        ? finishReplay(state, null)
        : state;
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
      const sync =
        state.sync.kind === "fallback" && !state.sync.preserveSelection
          ? { ...state.sync, preserveSelection: true }
          : state.sync;
      if (
        state.selectedKey === event.key &&
        unread === state.unread &&
        sync === state.sync
      )
        return state;
      return { ...state, selectedKey: event.key, unread, sync };
    }
    case "opened":
      return {
        ...state,
        sync: {
          kind: "replaying",
          status: state.viewKnown ? "reconnecting" : "connecting",
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
      channelCollator.compare(a.displayName, b.displayName) ||
      channelCollator.compare(a.workspaceId, b.workspaceId) ||
      channelCollator.compare(a.key, b.key),
  );
}
export function channelName(
  channel: ChannelSnapshot,
  channels: readonly ChannelSnapshot[],
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
  if (state.sync.kind === "replaying") return state.sync.status;
  if (state.sync.kind === "fallback")
    return state.viewKnown ? "live" : "connecting";
  return state.sync.kind;
}

export type ThreadGroup = {
  id: string;
  parents: readonly Message[];
  replies: readonly Message[];
  knownCount: number;
};
export function threadGroups(
  channel: Channel,
): ReadonlyMap<string, ThreadGroup> {
  const groups = new Map<
    string,
    { id: string; parents: Message[]; replies: Message[]; knownCount: number }
  >();
  for (const id of channel.runs.flat()) {
    const row = channel.messages.get(id);
    if (!row) continue;
    let group = groups.get(row.threadId);
    if (!group) {
      group = {
        id: row.threadId,
        parents: [],
        replies: [],
        knownCount: channel.threads.get(row.threadId)?.messageCount ?? 0,
      };
      groups.set(row.threadId, group);
    }
    if (row.inReplyTo !== null || row.kind === "reply") group.replies.push(row);
    else group.parents.push(row);
  }
  return groups;
}
export type TimelineRow =
  | { kind: "message"; message: Message; thread: ThreadGroup }
  | { kind: "orphan"; message: Message; thread: ThreadGroup }
  | { kind: "gap"; before: string };
export function timelineView(channel: Channel): readonly TimelineRow[] {
  const groups = threadGroups(channel);
  const rows: TimelineRow[] = [];
  for (const [index, run] of channel.runs.entries()) {
    const before = run[0];
    if (index > 0 && before !== undefined) rows.push({ kind: "gap", before });
    for (const id of run) {
      const message = channel.messages.get(id);
      if (!message) continue;
      const thread = groups.get(message.threadId);
      if (!thread) continue;
      if (message.inReplyTo !== null || message.kind === "reply") {
        if (thread.parents.length === 0)
          rows.push({ kind: "orphan", message, thread });
      } else rows.push({ kind: "message", message, thread });
    }
  }
  return rows;
}
