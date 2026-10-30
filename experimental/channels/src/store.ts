import {
  channelMention,
  channelToken,
  dayBounds,
  dayLabel,
  participantCopy,
} from "./copy.ts";
import type {
  Activity,
  AddressId,
  Channel,
  ChannelSnapshot,
  ComposerId,
  Directory,
  DraftKey,
  Event,
  HistoryRequest,
  Home,
  Landing,
  Member,
  Message,
  Outgoing,
  Participant,
  PeekableKind,
  PeekTarget,
  Route,
  Routing,
  Selection,
  Slot,
  SpaceKey,
  State,
  TerminalRun,
} from "./model.ts";
import { samePeek } from "./peek.ts";
import { ENVOY } from "./wire.ts";

const channelCollator = new Intl.Collator();
const noSelection: Selection = { kind: "none", key: null };
const activitySelection: Selection = { kind: "activity", key: null };
const closedSlot: Slot = { kind: "closed" };
const discovered: Landing = { kind: "awaiting-url" };
const settled: Landing = { kind: "settled" };

export function initialState({
  visible = true,
  requestedKey = null,
}: {
  visible?: boolean;
  requestedKey?: SpaceKey | null;
} = {}): State {
  return {
    channels: new Map(),
    selection: noSelection,
    landing: { kind: "discovering" },
    requestedKey,
    slot: closedSlot,
    unread: new Set(),
    sync: { kind: "connecting" },
    visible,
    viewKnown: false,
    nextRequestId: 1,
    outbox: new Map(),
    drafts: new Map(),
  };
}
export function bootState({
  visible,
  route,
}: {
  visible: boolean;
  route: Route;
}): State {
  const state = initialState({
    visible,
    requestedKey: route.kind === "channel" ? route.key : null,
  });
  return route.kind === "activity"
    ? reduce(state, { type: "activity" })
    : state;
}
export function routeEvent(route: Route): Event {
  switch (route.kind) {
    case "root":
      return { type: "navigate", key: null };
    case "channel":
      return { type: "navigate", key: route.key };
    case "activity":
      return { type: "activity" };
    default: {
      const exhaustive: never = route;
      return exhaustive;
    }
  }
}
export const HISTORY_CAP = 600;
const SNAPSHOT_CAP = 150;
function sameValue(a: unknown, b: unknown): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}
function sameRouting(a: Routing | null, b: Routing | null): boolean {
  if (a === null || b === null) return a === b;
  if (!sameList(a.mentioned, b.mentioned)) return false;
  if (a.state !== "routed" || b.state !== "routed") return a.state === b.state;
  return a.reason === b.reason && a.router === b.router;
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
      }) &&
      sameRouting(a.routing, b.routing) &&
      sameList(a.woke, b.woke))
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
function reuseRows<T>(
  previous: readonly T[] | undefined,
  incoming: readonly T[],
): readonly T[] {
  if (!previous) return incoming;
  if (sameValue(previous, incoming)) return previous;
  const known = new Map(previous.map((row) => [JSON.stringify(row), row]));
  return incoming.map((row) => known.get(JSON.stringify(row)) ?? row);
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
function refreshShown(
  shown: ReadonlyMap<string, Message>,
  fresh: ReadonlyMap<string, Message>,
): ReadonlyMap<string, Message> {
  let changed: Map<string, Message> | undefined;
  for (const [id, row] of fresh) {
    if (!shown.has(id) || shown.get(id) === row) continue;
    changed ??= new Map(shown);
    changed.set(id, row);
  }
  return changed ?? shown;
}
function fitLiveWindow(previous: Channel | undefined, next: Channel): Channel {
  if (
    next.messages.size > HISTORY_CAP &&
    previous?.viewMode === "following" &&
    !previous.atBottom
  )
    return {
      ...next,
      messages: refreshShown(previous.messages, next.messages),
      runs: previous.runs,
      viewMode: "browsing",
    };
  return capWindow(next, "oldest");
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
  const channel: Channel = fitLiveWindow(previous, {
    ...snapshot,
    health:
      previous && sameValue(previous.health, snapshot.health)
        ? previous.health
        : snapshot.health,
    attention:
      previous && sameValue(previous.attention, snapshot.attention)
        ? previous.attention
        : snapshot.attention,
    terminalRuns: reuseRows(previous?.terminalRuns, snapshot.terminalRuns),
    members: reuseMap(previous?.members, snapshot.members),
    work: reuseMap(previous?.work, snapshot.work),
    threads: reuseMap(previous?.threads, snapshot.threads),
    messages,
    latestMessages,
    viewMode: previous?.viewMode ?? "following",
    atBottom: previous?.atBottom ?? true,
    runs,
    history: previous?.history ?? { kind: "idle" },
    exhausted: previous?.exhausted ?? false,
  });
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
    const messages = refreshShown(merged.messages, pending.messages);
    return messages === merged.messages ? merged : { ...merged, messages };
  }
  let runs = previous.runs;
  for (const run of [...pending.runs, [...pending.latestMessages.keys()]])
    runs = mergeContiguousRuns(runs, run);
  let messages = previous.messages;
  let changed: Map<string, Message> | undefined;
  for (const rows of [pending.messages, pending.latestMessages])
    for (const [id, row] of rows) {
      if (messages.get(id) === row) continue;
      changed ??= new Map(messages);
      changed.set(id, row);
    }
  if (changed) messages = changed;
  if (sameRuns(merged.runs, runs)) runs = merged.runs;
  return messages === merged.messages && runs === merged.runs
    ? merged
    : fitLiveWindow(previous, { ...merged, messages, runs });
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
  const arrived = [...channels.keys()].some((key) => !state.channels.has(key));
  const selection = settle(state, channels, arrived);
  const unread = new Set([...state.unread].filter((key) => channels.has(key)));
  for (const [key, channel] of channels) {
    const previous = state.channels.get(key);
    if (
      previous &&
      hasActivity(previous, channel) &&
      !(state.visible && key === state.selection.key)
    )
      unread.add(key);
  }
  return {
    ...state,
    channels,
    selection,
    unread,
    outbox: settleOutbox(state.outbox, [...channels.values()]),
    viewKnown: state.viewKnown || channels.size > 0 || keys !== null,
    sync: { kind: keys === null ? "fallback" : "live" },
    landing:
      keys !== null && state.landing.kind === "discovering"
        ? discovered
        : state.landing,
  };
}
function settle(
  state: State,
  channels: ReadonlyMap<SpaceKey, Channel>,
  arrived: boolean,
): Selection {
  const { selection, landing, requestedKey } = state;
  switch (selection.kind) {
    case "activity":
      return selection;
    case "none":
      return arrived ? land(channels, requestedKey) : selection;
    case "automatic":
    case "chosen":
      if (!channels.has(selection.key)) return land(channels, requestedKey);
      if (!arrived || selection.kind === "chosen") return selection;
      switch (landing.kind) {
        case "discovering":
          return land(channels, requestedKey);
        case "awaiting-url":
          return requestedKey !== null && channels.has(requestedKey)
            ? { kind: "chosen", key: requestedKey }
            : selection;
        case "settled":
          return selection;
        default: {
          const exhaustive: never = landing;
          return exhaustive;
        }
      }
    default: {
      const exhaustive: never = selection;
      return exhaustive;
    }
  }
}
function land(
  channels: ReadonlyMap<SpaceKey, Channel>,
  requestedKey: SpaceKey | null,
): Selection {
  const key = landing(channels, requestedKey);
  if (key === null) return noSelection;
  return { kind: key === requestedKey ? "chosen" : "automatic", key };
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
    atBottom: true,
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
  return settleSlot(state, step(state, event));
}
export function viewOf(selection: Selection): SpaceKey | "activity" | null {
  return selection.kind === "activity" ? "activity" : selection.key;
}
function settleSlot(before: State, after: State): State {
  const { slot } = after;
  if (slot.kind === "closed") return after;
  if (viewOf(after.selection) !== viewOf(before.selection))
    return { ...after, slot: closedSlot };
  switch (slot.kind) {
    case "thread": {
      const key = after.selection.key;
      if (key === null) return after;
      const channel = after.channels.get(key);
      return channel === undefined ||
        channel.messages === before.channels.get(key)?.messages ||
        threadLoaded(channel, slot.id)
        ? after
        : { ...after, slot: closedSlot };
    }
    case "peek":
      return after.channels.has(slot.target.channel)
        ? after
        : { ...after, slot: closedSlot };
    default: {
      const exhaustive: never = slot;
      return exhaustive;
    }
  }
}
function threadLoaded(channel: Channel, threadId: string): boolean {
  for (const run of channel.runs)
    for (const id of run)
      if (channel.messages.get(id)?.threadId === threadId) return true;
  return false;
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
      const selection = settle(state, channels, previous === undefined);
      let unread = state.unread;
      if (
        previous &&
        hasActivity(previous, accumulated) &&
        !(selection.key === channel.key && state.visible)
      )
        unread = new Set([...unread, channel.key]);
      return {
        ...state,
        channels,
        selection,
        unread,
        outbox: settleOutbox(state.outbox, [accumulated]),
        viewKnown: true,
      };
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
    case "follow": {
      const channel = state.channels.get(event.key);
      if (channel === undefined || channel.atBottom === event.atBottom)
        return state;
      const channels = new Map(state.channels);
      channels.set(event.key, { ...channel, atBottom: event.atBottom });
      return { ...state, channels };
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
    case "discovery-timeout":
      return state.landing.kind === "discovering"
        ? { ...state, landing: discovered }
        : state;
    case "input":
      return state.landing.kind === "settled"
        ? state
        : { ...state, landing: settled };
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
        selection:
          state.selection.key === event.key ? noSelection : state.selection,
        unread: clearUnread(state.unread, event.key),
      };
    }
    case "select": {
      if (!state.channels.has(event.key)) return state;
      const { selection } = state;
      if (
        selection.kind === "chosen" &&
        selection.key === event.key &&
        state.landing.kind === "settled"
      )
        return state;
      return {
        ...state,
        requestedKey: event.key,
        selection: { kind: "chosen", key: event.key },
        landing: settled,
      };
    }
    case "navigate": {
      if (event.key !== null && !state.channels.has(event.key)) return state;
      return {
        ...state,
        requestedKey: event.key,
        selection: land(state.channels, event.key),
      };
    }
    case "read": {
      if (state.selection.key !== event.key) return state;
      const unread = clearUnread(state.unread, event.key);
      return unread === state.unread ? state : { ...state, unread };
    }
    case "thread": {
      const { slot } = state;
      if (event.threadId === null)
        return slot.kind === "thread" ? { ...state, slot: closedSlot } : state;
      return slot.kind === "thread" && slot.id === event.threadId
        ? state
        : { ...state, slot: { kind: "thread", id: event.threadId } };
    }
    case "peek": {
      const { slot } = state;
      const { target } = event;
      if (target === null)
        return slot.kind === "peek" ? { ...state, slot: closedSlot } : state;
      if (
        !state.channels.has(target.channel) ||
        (slot.kind === "peek" && samePeek(slot.target, target))
      )
        return state;
      return { ...state, slot: { kind: "peek", target } };
    }
    case "activity":
      return state.selection.kind === "activity" &&
        state.landing.kind === "settled"
        ? state
        : { ...state, selection: activitySelection, landing: settled };
    case "jump": {
      const chosen = step(state, { type: "select", key: event.key });
      return chosen.channels.get(event.key)?.viewMode === "browsing"
        ? step(chosen, { type: "latest", key: event.key })
        : chosen;
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
    case "visibility":
      return state.visible === event.visible
        ? state
        : { ...state, visible: event.visible };
    case "edit": {
      const at = draftKey(event.composer);
      const known = state.drafts.get(at);
      if (
        known === undefined
          ? event.text === ""
          : known.text === event.text && known.failure === null
      )
        return state;
      const drafts = new Map(state.drafts);
      const attempt = known?.attempt ?? null;
      if (event.text === "" && attempt === null) drafts.delete(at);
      else
        drafts.set(at, {
          ...event.composer,
          text: event.text,
          attempt,
          failure: null,
        });
      return { ...state, drafts };
    }
    case "fill": {
      const known = state.drafts.get(draftKey(event.composer));
      return known === undefined || known.text === ""
        ? step(state, { ...event, type: "edit" })
        : state;
    }
    case "send": {
      const { outgoing } = event;
      if (state.outbox.has(outgoing.requestId)) return state;
      const outbox = new Map(state.outbox);
      outbox.set(outgoing.requestId, {
        ...outgoing,
        phase: { kind: "sending" },
      });
      const at = draftKey(outgoing.composer);
      const draft = state.drafts.get(at);
      return {
        ...state,
        outbox,
        drafts: draft?.failure
          ? new Map(state.drafts).set(at, { ...draft, failure: null })
          : state.drafts,
      };
    }
    case "sent": {
      const known = state.outbox.get(event.requestId);
      if (known?.phase.kind !== "sending") return state;
      const outbox = new Map(state.outbox);
      outbox.set(event.requestId, {
        ...known,
        phase: { kind: "sent", messageId: event.messageId },
      });
      const drafts = new Map(state.drafts);
      drafts.delete(draftKey(known.composer));
      return {
        ...state,
        outbox: settleOutbox(outbox, [...state.channels.values()]),
        drafts,
      };
    }
    case "send-failed": {
      const known = state.outbox.get(event.requestId);
      if (known?.phase.kind !== "sending") return state;
      const outbox = new Map(state.outbox);
      outbox.delete(event.requestId);
      const at = draftKey(known.composer);
      const drafts = new Map(state.drafts);
      drafts.set(at, {
        ...known.composer,
        text: state.drafts.get(at)?.text ?? known.source,
        attempt: {
          requestId: event.requestId,
          source: known.source,
          inReplyTo: known.inReplyTo,
        },
        failure: { status: event.status, message: event.message },
      });
      return { ...state, outbox, drafts };
    }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
function settleOutbox(
  outbox: ReadonlyMap<string, Outgoing>,
  channels: readonly Channel[],
): ReadonlyMap<string, Outgoing> {
  let settled: Map<string, Outgoing> | undefined;
  for (const [requestId, { phase }] of outbox) {
    if (phase.kind !== "sent") continue;
    const arrived = channels.some(
      (channel) =>
        channel.messages.has(phase.messageId) ||
        channel.latestMessages.has(phase.messageId),
    );
    if (!arrived) continue;
    settled ??= new Map(outbox);
    settled.delete(requestId);
  }
  return settled ?? outbox;
}
export function draftKey({ key, thread }: ComposerId): DraftKey {
  return thread === null ? `channel:${key}` : `thread:${thread}`;
}
export function sameComposer(a: ComposerId, b: ComposerId): boolean {
  return a.key === b.key && a.thread === b.thread;
}
export function inFlight(
  outbox: ReadonlyMap<string, Outgoing>,
  composer: ComposerId,
): boolean {
  for (const entry of outbox.values())
    if (
      entry.phase.kind === "sending" &&
      sameComposer(entry.composer, composer)
    )
      return true;
  return false;
}
export function compareChannels(
  a: Pick<ChannelSnapshot, "displayName" | "workspaceId" | "key">,
  b: Pick<ChannelSnapshot, "displayName" | "workspaceId" | "key">,
): number {
  return (
    channelCollator.compare(a.displayName, b.displayName) ||
    channelCollator.compare(a.workspaceId, b.workspaceId) ||
    channelCollator.compare(a.key, b.key)
  );
}
export function sortedChannels(state: {
  channels: ReadonlyMap<SpaceKey, Channel>;
}): Channel[] {
  return [...state.channels.values()].sort(compareChannels);
}
export function channelSuffix(
  channel: Pick<ChannelSnapshot, "key" | "displayName" | "workspaceId">,
  channels: Iterable<Pick<ChannelSnapshot, "displayName">>,
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

// The latest time each author wrote anything in a thread.
export type Wrote = ReadonlyMap<AddressId | "human", number>;
// A thread that reads as a conversation, every message its own row. Its first
// loaded parent decides: a channel post makes a channel room, a first message
// an agent wrote makes an agents room, and anything else is a request thread.
// Everyone a channel thread sent a message to, so an agent who already has the
// conversation is never offered it again.
export type Heard = ReadonlySet<AddressId>;
export type Room =
  | { kind: "channel"; post: Message; wrote: Wrote; heard: Heard }
  | { kind: "agents"; root: Message; wrote: Wrote };
export type ThreadGroup = {
  id: string;
  parents: readonly Message[];
  replies: readonly Message[];
  knownCount: number;
  room: Room | null;
};
function agentAuthor(
  author: Message["author"],
  members: ReadonlyMap<AddressId, Member>,
): boolean {
  if (author === "human") return false;
  const type = members.get(author)?.kind.type;
  return type !== "terminal" && type !== "human";
}
function wroteIn(group: Pick<ThreadGroup, "parents" | "replies">): Wrote {
  const wrote = new Map<AddressId | "human", number>();
  for (const messages of [group.parents, group.replies])
    for (const { author, at } of messages)
      wrote.set(author, Math.max(at, wrote.get(author) ?? at));
  return wrote;
}
function heardIn(group: Pick<ThreadGroup, "parents" | "replies">): Heard {
  const heard = new Set<AddressId>();
  for (const messages of [group.parents, group.replies])
    for (const { legs } of messages) for (const { to } of legs) heard.add(to);
  return heard;
}
export function roomOf(
  group: Pick<ThreadGroup, "parents" | "replies">,
  members: ReadonlyMap<AddressId, Member>,
): Room | null {
  const [first] = group.parents;
  if (first === undefined) return null;
  if (first.routing !== null)
    return {
      kind: "channel",
      post: first,
      wrote: wroteIn(group),
      heard: heardIn(group),
    };
  return agentAuthor(first.author, members)
    ? { kind: "agents", root: first, wrote: wroteIn(group) }
    : null;
}
export function threadGroups(
  channel: Pick<Channel, "messages" | "runs" | "threads" | "members">,
): ReadonlyMap<string, ThreadGroup> {
  const groups = new Map<
    string,
    {
      id: string;
      parents: Message[];
      replies: Message[];
      knownCount: number;
      room: Room | null;
    }
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
        room: null,
      };
      groups.set(row.threadId, group);
    }
    if (row.inReplyTo !== null || row.kind === "reply") group.replies.push(row);
    else group.parents.push(row);
  }
  for (const group of groups.values())
    group.room = roomOf(group, channel.members);
  return groups;
}
// The line above a conversation reply whose row above is not the message it answers.
export type ReplyReference =
  | { kind: "message"; parent: Message }
  | { kind: "earlier" };
const earlier: ReplyReference = { kind: "earlier" };
export type MessageRow = {
  kind: "message" | "orphan";
  message: Message;
  thread: ThreadGroup;
  grouped: boolean;
  showRecipients: boolean;
  recipients: readonly AddressId[];
  latestReply: Message | null;
  replyCount: number;
  reference: ReplyReference | null;
};
export type TimelineRow =
  | MessageRow
  | { kind: "gap"; before: string }
  | { kind: "day"; label: string; before: string };
export const GROUP_WINDOW = 5 * 60_000;
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
  channel: Pick<Channel, "messages" | "runs" | "threads" | "members">,
  groups: ReadonlyMap<string, ThreadGroup> = threadGroups(channel),
  now = Date.now(),
): readonly TimelineRow[] {
  const rows: TimelineRow[] = [];
  let dayStart = Number.POSITIVE_INFINITY;
  let dayEnd = Number.NEGATIVE_INFINITY;
  let previous: Message | null = null;
  // A channel-chat header names no one, so a row grouped under it compares with nobody.
  let previousNamed = noRecipients;
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
      const { room } = thread;
      const reply = message.inReplyTo !== null || message.kind === "reply";
      if (reply && room === null && thread.parents.length > 0) continue;
      if (message.at < dayStart || message.at >= dayEnd) {
        ({ start: dayStart, end: dayEnd } = dayBounds(message.at));
        rows.push({ kind: "day", label: dayLabel(dayStart, now), before: id });
        previous = null;
      }
      const to = recipients(message);
      let reference: ReplyReference | null = null;
      if (
        room !== null &&
        message.inReplyTo !== null &&
        previous?.id !== message.inReplyTo
      ) {
        const parent = channel.messages.get(message.inReplyTo);
        reference =
          parent === undefined ? earlier : { kind: "message", parent };
      }
      const grouped =
        (!reply || room !== null) &&
        reference === null &&
        previous !== null &&
        previous.author === message.author &&
        message.at - previous.at >= 0 &&
        message.at - previous.at <= GROUP_WINDOW &&
        !isSystem(message) &&
        !isSystem(previous);
      const channelChat = room?.kind === "channel";
      rows.push({
        kind: reply && room === null ? "orphan" : "message",
        message,
        thread,
        grouped,
        showRecipients: channelChat
          ? !grouped
          : !grouped ||
            ((to.length > 0 || message.kind === "handoff") &&
              (previous?.kind !== message.kind ||
                !sameList(previousNamed, to))),
        recipients: to,
        latestReply:
          reply || room !== null ? null : (thread.replies.at(-1) ?? null),
        replyCount: reply
          ? 0
          : Math.max(
              thread.replies.length,
              thread.knownCount - Math.max(1, thread.parents.length),
            ),
        reference,
      });
      previous = reply && room === null ? null : message;
      previousNamed = channelChat ? noRecipients : to;
    }
  }
  return rows;
}
export type RunIndex = ReadonlyMap<string, readonly TerminalRun[]>;
export function runsByMessage(runs: readonly TerminalRun[]): RunIndex {
  const byMessage = new Map<string, TerminalRun[]>();
  for (const run of runs) {
    if (run.messageId === null) continue;
    const known = byMessage.get(run.messageId);
    if (known) known.push(run);
    else byMessage.set(run.messageId, [run]);
  }
  for (const list of byMessage.values())
    list.sort((a, b) => b.startedAt - a.startedAt);
  return byMessage;
}
export function exitFor(
  runs: RunIndex,
  reply: Message | null,
): TerminalRun | undefined {
  if (reply === null || reply.inReplyTo === null) return undefined;
  return runs
    .get(reply.inReplyTo)
    ?.find((run) => run.terminal === reply.author && run.startedAt <= reply.at);
}
export function homesOf(
  channels: Iterable<ChannelSnapshot>,
  previous: ReadonlyMap<string, Home> | undefined,
): ReadonlyMap<string, Home> {
  const homes = new Map<string, Home>();
  for (const channel of channels) {
    if (homes.has(channel.workspaceId)) continue;
    const name = channelMention(channel);
    const token = channelToken(channel);
    const old = previous?.get(channel.workspaceId);
    homes.set(
      channel.workspaceId,
      old?.key === channel.key &&
        old.name === name &&
        old.token === token &&
        old.members === channel.members
        ? old
        : { key: channel.key, name, token, members: channel.members },
    );
  }
  return previous !== undefined &&
    previous.size === homes.size &&
    [...homes].every(([id, home]) => previous.get(id) === home)
    ? previous
    : homes;
}
export function participant(
  { members, homes }: Directory,
  id: AddressId | "human",
): Participant {
  if (id === "human") return { kind: "operator" };
  const member = members.get(id);
  if (member !== undefined)
    return member.kind.type === "human"
      ? { kind: "operator" }
      : { kind: "member", member, home: null };
  const [, workspace, origin] = ENVOY.exec(id) ?? [];
  if (workspace === undefined || origin === undefined)
    return { kind: "unknown", id };
  const home = homes.get(workspace);
  const agent = home?.members.get(origin);
  return home !== undefined && agent !== undefined
    ? { kind: "member", member: agent, home }
    : { kind: "envoy", id, home: home ?? null };
}
// Who a thread reply goes to when its text names no one: every agent who wrote
// in the thread or was sent a message in it, as the server reads the thread.
export function threadAgents(
  thread: ThreadGroup,
  directory: Directory,
): readonly string[] {
  const names = new Map<AddressId | "human", string>();
  for (const message of [...thread.parents, ...thread.replies])
    for (const id of [message.author, ...recipients(message)]) {
      if (names.has(id)) continue;
      const who = participant(directory, id);
      const agent =
        who.kind === "envoy" ||
        (who.kind === "member" &&
          who.member.kind.type !== "terminal" &&
          who.member.activity !== "retired");
      if (!agent) continue;
      const { name, suffix } = participantCopy(who);
      names.set(id, suffix === null ? name : `${name} · ${suffix}`);
    }
  return [...names.values()];
}

export type PeekWho = Readonly<{
  alias: string;
  kind: PeekableKind;
  activity: Activity | null;
  channel: { label: string; suffix: string | null };
}>;
export function peekWho(
  channels: ReadonlyMap<SpaceKey, Channel>,
  target: PeekTarget,
): PeekWho | null {
  const channel = channels.get(target.channel);
  if (channel === undefined) return null;
  const member = channel.members.get(target.member);
  const kind = member?.kind.type;
  return {
    alias: member?.alias ?? target.alias,
    kind: kind === undefined || kind === "human" ? target.kind : kind,
    activity: member?.activity ?? null,
    channel: {
      label: channelMention(channel),
      suffix: channelSuffix(channel, channels.values()),
    },
  };
}
