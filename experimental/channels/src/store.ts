import { dayBounds, dayLabel } from "./copy.ts";
import type {
  AddressId,
  Channel,
  ChannelSnapshot,
  Event,
  HistoryRequest,
  Message,
  SpaceKey,
  State,
  TerminalRun,
} from "./model.ts";

const channelCollator = new Intl.Collator();

export function initialState({
  visible = true,
  requestedKey = null,
}: {
  visible?: boolean;
  requestedKey?: SpaceKey | null;
} = {}): State {
  return {
    channels: new Map(),
    selectedKey: null,
    requestedKey,
    openThread: null,
    unread: new Set(),
    sync: { kind: "connecting" },
    visible,
    viewKnown: false,
    nextRequestId: 1,
  };
}
export const HISTORY_CAP = 600;
const SNAPSHOT_CAP = 150;
function sameValue(a: unknown, b: unknown): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}
function sameMessage(a: Message, b: Message): boolean {
  return (
    a === b ||
    (a.id === b.id &&
      a.threadId === b.threadId &&
      a.inReplyTo === b.inReplyTo &&
      a.at === b.at &&
      a.author === b.author &&
      a.kind === b.kind &&
      a.body.text === b.body.text &&
      a.body.truncated === b.body.truncated &&
      a.legs.length === b.legs.length &&
      a.legs.every((leg, index) => {
        const other = b.legs[index];
        return (
          other !== undefined &&
          leg.to === other.to &&
          sameValue(leg.stage, other.stage)
        );
      }))
  );
}
function reuseMap<K, V>(
  previous: ReadonlyMap<K, V> | undefined,
  incoming: ReadonlyMap<K, V>,
): ReadonlyMap<K, V> {
  if (
    previous &&
    previous.size === incoming.size &&
    [...incoming].every(([id, row]) => sameValue(previous.get(id), row))
  )
    return previous;
  return new Map(
    [...incoming].map(([id, row]) => {
      const old = previous?.get(id);
      return [id, old !== undefined && sameValue(old, row) ? old : row];
    }),
  );
}
function sameRuns(a: Channel["runs"], b: Channel["runs"]): boolean {
  return (
    a.length === b.length &&
    a.every((run, index) => {
      const other = b[index];
      return (
        other !== undefined &&
        run.length === other.length &&
        run.every((id, at) => id === other[at])
      );
    })
  );
}
function capWindow(channel: Channel, edge: "oldest" | "newest"): Channel {
  if (channel.messages.size <= HISTORY_CAP) return channel;
  const all = channel.runs.flat();
  const retained =
    edge === "oldest" ? all.slice(-HISTORY_CAP) : all.slice(0, HISTORY_CAP);
  const ids = new Set(retained);
  return {
    ...channel,
    messages: new Map(
      retained.flatMap((id) => {
        const row = channel.messages.get(id);
        return row ? [[id, row] as const] : [];
      }),
    ),
    runs: channel.runs
      .map((run) => run.filter((id) => ids.has(id)))
      .filter((run) => run.length > 0),
    viewMode: edge === "newest" ? "browsing" : channel.viewMode,
    exhausted: edge === "oldest" ? false : channel.exhausted,
    history:
      edge === "oldest" &&
      channel.history.kind === "loading" &&
      !ids.has(channel.history.request.before)
        ? { kind: "idle" }
        : channel.history,
  };
}
function mergeContiguousRuns(
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
  let inserted = false;
  for (let i = 0; i < incoming.length; i++) {
    const id = incoming[i];
    if (id === undefined || known.has(id)) continue;
    const next = incoming
      .slice(i + 1)
      .find((candidate) => known.has(candidate));
    const at = next === undefined ? joined.length : joined.indexOf(next);
    joined.splice(at, 0, id);
    known.add(id);
    inserted = true;
  }
  return first === last && !inserted
    ? runs
    : [...runs.slice(0, first), joined, ...runs.slice(last + 1)];
}
function mergeSnapshot(
  previous: Channel | undefined,
  snapshot: ChannelSnapshot,
): Channel {
  let messages = previous?.messages ?? new Map<string, Message>();
  let changed: Map<string, Message> | undefined;
  const latest = new Map<string, Message>();
  const incomingRows = [...snapshot.messages];
  const latestIds = new Set(
    incomingRows.slice(-SNAPSHOT_CAP).map(([id]) => id),
  );
  for (const [id, incoming] of incomingRows) {
    const old = previous?.latestMessages.get(id) ?? messages.get(id);
    const row = old && sameMessage(old, incoming) ? old : incoming;
    if (latestIds.has(id)) latest.set(id, row);
    if (
      (previous?.viewMode !== "browsing" || messages.has(id)) &&
      messages.get(id) !== row
    ) {
      changed ??= new Map(messages);
      changed.set(id, row);
    }
  }
  if (changed) messages = changed;
  const latestMessages =
    previous &&
    previous.latestMessages.size === latest.size &&
    [...latest].every(([id, row]) => previous.latestMessages.get(id) === row)
      ? previous.latestMessages
      : latest;
  const extendsCoverage =
    !previous || incomingRows.some(([id]) => !previous.latestMessages.has(id));
  const runs =
    previous && (!extendsCoverage || previous.viewMode === "browsing")
      ? previous.runs
      : mergeContiguousRuns(previous?.runs ?? [], [
          ...snapshot.messages.keys(),
        ]);
  const channel: Channel = capWindow(
    {
      ...snapshot,
      health:
        previous && sameValue(previous.health, snapshot.health)
          ? previous.health
          : snapshot.health,
      attention:
        previous && sameValue(previous.attention, snapshot.attention)
          ? previous.attention
          : snapshot.attention,
      terminalRuns:
        previous && sameValue(previous.terminalRuns, snapshot.terminalRuns)
          ? previous.terminalRuns
          : snapshot.terminalRuns,
      members: reuseMap(previous?.members, snapshot.members),
      threads: reuseMap(previous?.threads, snapshot.threads),
      messages,
      latestMessages,
      viewMode: previous?.viewMode ?? "following",
      runs,
      history: previous?.history ?? { kind: "idle" },
      exhausted: previous?.exhausted ?? false,
    },
    "oldest",
  );
  return previous &&
    Object.keys(channel).every(
      (key) => Reflect.get(previous, key) === Reflect.get(channel, key),
    )
    ? previous
    : channel;
}
function reconcile(previous: Channel | undefined, pending: Channel): Channel {
  if (!previous) return pending;
  const merged = mergeSnapshot(previous, {
    ...pending,
    messages: pending.latestMessages,
  });
  if (merged.viewMode === "browsing") {
    let changed: Map<string, Message> | undefined;
    for (const [id, row] of pending.messages) {
      if (!merged.messages.has(id) || merged.messages.get(id) === row) continue;
      changed ??= new Map(merged.messages);
      changed.set(id, row);
    }
    return changed ? { ...merged, messages: changed } : merged;
  }
  let runs = previous.runs;
  for (const run of pending.runs) runs = mergeContiguousRuns(runs, run);
  let messages = previous.messages;
  let changed: Map<string, Message> | undefined;
  for (const [id, row] of pending.messages) {
    if (messages.get(id) === row) continue;
    changed ??= new Map(messages);
    changed.set(id, row);
  }
  if (changed) messages = changed;
  if (sameRuns(merged.runs, runs)) runs = merged.runs;
  return messages === merged.messages && runs === merged.runs
    ? merged
    : capWindow({ ...merged, messages, runs }, "oldest");
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
    [...b.latestMessages.keys()].some(
      (id) => !a.latestMessages.has(id) && !a.messages.has(id),
    ) ||
    a.attention !== b.attention ||
    a.members.size !== b.members.size ||
    [...b.members].some(
      ([id, member]) => a.members.get(id)?.activity !== member.activity,
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
      : landing(channels, state.requestedKey);
  const unread = new Set([...state.unread].filter((key) => channels.has(key)));
  for (const [key, channel] of channels) {
    const previous = state.channels.get(key);
    if (
      previous &&
      hasActivity(previous, channel) &&
      !(state.visible && key === state.selectedKey)
    )
      unread.add(key);
  }
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
function latestWindow(channel: Channel): Channel {
  return {
    ...channel,
    messages: channel.latestMessages,
    runs:
      channel.latestMessages.size > 0
        ? [[...channel.latestMessages.keys()]]
        : [],
    viewMode: "following",
    history: { kind: "idle" },
    exhausted: false,
  };
}
export function landing(
  channels: ReadonlyMap<SpaceKey, Channel>,
  requestedKey: SpaceKey | null,
): SpaceKey | null {
  if (requestedKey !== null && channels.has(requestedKey)) return requestedKey;
  const sorted = sortedChannels({ channels });
  const needsYou = sorted.find(
    (channel) => channel.health.level === "needs_you",
  );
  if (needsYou) return needsYou.key;
  let latest = sorted[0];
  let latestAt = Number.NEGATIVE_INFINITY;
  for (const channel of sorted)
    for (const message of channel.latestMessages.values())
      if (message.at > latestAt) {
        latestAt = message.at;
        latest = channel;
      }
  return latest?.key ?? null;
}
export function reduce(state: State, event: Event): State {
  const next = step(state, event);
  return next.openThread !== null && next.selectedKey !== state.selectedKey
    ? { ...next, openThread: null }
    : next;
}
function step(state: State, event: Event): State {
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
    case "latest": {
      const channel = state.channels.get(event.key);
      if (!channel) return state;
      const channels = new Map(state.channels);
      channels.set(event.key, latestWindow(channel));
      let sync = state.sync;
      if (sync.kind === "replaying") {
        const staged = sync.pending.get(event.key);
        if (staged) {
          const pending = new Map(sync.pending);
          pending.set(event.key, latestWindow(staged));
          sync = { ...sync, pending };
        }
      }
      return { ...state, channels, sync };
    }
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
        next = capWindow(
          {
            ...channel,
            messages,
            runs: mergeContiguousRuns(channel.runs, [
              ...event.result.messages.map((row) => row.id),
              event.request.before,
            ]),
            history: { kind: "idle" },
            exhausted:
              channel.exhausted ||
              (event.request.purpose === "older" &&
                event.result.messages.length === 0),
          },
          "newest",
        );
        if (next.runs.length > 1 && sameRuns(next.runs, channel.runs))
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
      const sync =
        state.sync.kind === "fallback" && !state.sync.preserveSelection
          ? { ...state.sync, preserveSelection: true }
          : state.sync;
      if (state.selectedKey === event.key && sync === state.sync) return state;
      return { ...state, selectedKey: event.key, sync };
    }
    case "read": {
      if (state.selectedKey !== event.key) return state;
      const unread = clearUnread(state.unread, event.key);
      return unread === state.unread ? state : { ...state, unread };
    }
    case "thread":
      return state.openThread === event.threadId
        ? state
        : { ...state, openThread: event.threadId };
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
    case "visibility":
      return state.visible === event.visible
        ? state
        : { ...state, visible: event.visible };
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
export function sortedChannels(state: {
  channels: ReadonlyMap<SpaceKey, Channel>;
}): Channel[] {
  return [...state.channels.values()].sort(
    (a, b) =>
      channelCollator.compare(a.displayName, b.displayName) ||
      channelCollator.compare(a.workspaceId, b.workspaceId) ||
      channelCollator.compare(a.key, b.key),
  );
}
export function channelSuffix(
  channel: ChannelSnapshot,
  channels: Iterable<ChannelSnapshot>,
): string | null {
  let sameName = 0;
  for (const other of channels)
    if (other.displayName === channel.displayName) sameName++;
  return sameName < 2 ? null : `${channel.workspaceId} · ${channel.key}`;
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
  channel: Pick<Channel, "messages" | "runs" | "threads">,
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
export type MessageRow = {
  kind: "message" | "orphan";
  message: Message;
  thread: ThreadGroup;
  grouped: boolean;
  showRecipients: boolean;
  recipients: readonly AddressId[];
  latestReply: Message | null;
  replyCount: number;
};
export type TimelineRow =
  | MessageRow
  | { kind: "gap"; before: string }
  | { kind: "day"; label: string; before: string };
const GROUP_WINDOW = 5 * 60_000;
const noRecipients: readonly AddressId[] = [];
const recipientCache = new WeakMap<Message, readonly AddressId[]>();
export function recipients(message: Message): readonly AddressId[] {
  let cached = recipientCache.get(message);
  if (cached === undefined) {
    cached =
      message.legs.length === 0
        ? noRecipients
        : [...new Set(message.legs.map((leg) => leg.to))];
    recipientCache.set(message, cached);
  }
  return cached;
}
function sameList<T>(a: readonly T[], b: readonly T[]): boolean {
  return a === b || (a.length === b.length && a.every((x, i) => x === b[i]));
}
function isSystem(message: Message): boolean {
  return message.kind === "nudge" || message.kind === "hook";
}
export function timelineView(
  channel: Pick<Channel, "messages" | "runs" | "threads">,
  groups: ReadonlyMap<string, ThreadGroup> = threadGroups(channel),
  now = Date.now(),
): readonly TimelineRow[] {
  const rows: TimelineRow[] = [];
  let dayStart = Number.POSITIVE_INFINITY;
  let dayEnd = Number.NEGATIVE_INFINITY;
  let previous: Message | null = null;
  for (const [index, run] of channel.runs.entries()) {
    const before = run[0];
    if (index > 0 && before !== undefined) {
      rows.push({ kind: "gap", before });
      previous = null;
    }
    for (const id of run) {
      const message = channel.messages.get(id);
      if (!message) continue;
      const thread = groups.get(message.threadId);
      if (!thread) continue;
      const reply = message.inReplyTo !== null || message.kind === "reply";
      if (reply && thread.parents.length > 0) continue;
      if (message.at < dayStart || message.at >= dayEnd) {
        ({ start: dayStart, end: dayEnd } = dayBounds(message.at));
        rows.push({ kind: "day", label: dayLabel(dayStart, now), before: id });
        previous = null;
      }
      const to = recipients(message);
      const grouped =
        !reply &&
        previous !== null &&
        previous.author === message.author &&
        message.at - previous.at >= 0 &&
        message.at - previous.at <= GROUP_WINDOW &&
        !isSystem(message) &&
        !isSystem(previous);
      rows.push({
        kind: reply ? "orphan" : "message",
        message,
        thread,
        grouped,
        showRecipients:
          !grouped ||
          ((to.length > 0 || message.kind === "handoff") &&
            (previous?.kind !== message.kind ||
              !sameList(recipients(previous), to))),
        recipients: to,
        latestReply: reply ? null : (thread.replies.at(-1) ?? null),
        replyCount: reply
          ? 0
          : Math.max(
              thread.replies.length,
              thread.knownCount - Math.max(1, thread.parents.length),
            ),
      });
      previous = reply ? null : message;
    }
  }
  return rows;
}
export function runsByMessage(
  runs: readonly TerminalRun[],
): ReadonlyMap<string, TerminalRun> {
  const byMessage = new Map<string, TerminalRun>();
  for (const run of runs)
    if (run.messageId !== null) byMessage.set(run.messageId, run);
  return byMessage;
}
export function exitFor(
  runs: ReadonlyMap<string, TerminalRun>,
  reply: Message | null,
): TerminalRun | undefined {
  if (reply === null || reply.inReplyTo === null) return undefined;
  const run = runs.get(reply.inReplyTo);
  return run?.terminal === reply.author ? run : undefined;
}
