import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import { type ActivityList, activityOf } from "../activity.ts";
import {
  evidenceReduce,
  evidenceThread,
  prepareChannelMessage,
} from "../attention.ts";
import { type CompletionContext, channelOptions } from "../complete.ts";
import { composerPlaceholder } from "../copy.ts";
import type {
  ComposerId,
  Directory,
  Event,
  Home,
  Message,
  Outgoing,
  PeekTarget,
  Route,
  SendRule,
  SpaceKey,
  State,
} from "../model.ts";
import { type PeekKey, peekKey } from "../peek.ts";
import { createScreenReader } from "../screen.ts";
import { createPoster } from "../send.ts";
import {
  bootState,
  channelSuffix,
  connectionStatus,
  draftKey,
  homesOf,
  peekWho,
  reduce,
  routeEvent,
  runsByMessage,
  sameComposer,
  type ThreadGroup,
  type TimelineRow,
  threadGroups,
  timelineView,
  viewOf,
} from "../store.ts";
import {
  parseEvent,
  parseHistory,
  parseMessageContext,
  routeOf,
  routePath,
  sameRoute,
} from "../wire.ts";
import { ActivityView } from "./ActivityView.tsx";
import type { FocusAttention } from "./AttentionActions.tsx";
import { ChannelHeader } from "./ChannelHeader.tsx";
import { Composer } from "./Composer.tsx";
import { Icon } from "./Icons.tsx";
import { MemberStrip } from "./MemberStrip.tsx";
import { measure } from "./measure.ts";
import { NeedsYou } from "./NeedsYou.tsx";
import { type PeekAccess, PeekContext } from "./PeekContext.tsx";
import { PeekDrawer } from "./PeekDrawer.tsx";
import { Sidebar } from "./Sidebar.tsx";
import { ThreadPane, type ThreadResponse } from "./ThreadPane.tsx";
import { type Located, Timeline } from "./Timeline.tsx";

const noGroups: ReadonlyMap<string, ThreadGroup> = new Map();
const noRows: readonly TimelineRow[] = [];
const noPending: readonly Outgoing[] = [];
const noMembers: Directory["members"] = new Map();
const POSTED_LIMIT = 200;
const MAIN_SCROLLER = "main .timeline, main .activity-list";
function go(route: Route) {
  if (!sameRoute(routeOf(location), route))
    history.pushState(null, "", routePath(route));
}
function measuredReduce(state: State, event: Event): State {
  if (
    event.type !== "space" &&
    event.type !== "gone" &&
    event.type !== "ready" &&
    event.type !== "replay-timeout"
  )
    return reduce(state, event);
  performance.mark("namedrop:frame-start");
  const next = reduce(state, event);
  performance.mark("namedrop:frame-end");
  const name = "namedrop:frame-apply";
  if (performance.getEntriesByName(name, "measure").length >= 200)
    performance.clearMeasures(name);
  performance.measure(name, "namedrop:frame-start", "namedrop:frame-end");
  performance.clearMarks("namedrop:frame-start");
  performance.clearMarks("namedrop:frame-end");
  return next;
}
export function App() {
  const [state, dispatch] = useReducer(measuredReduce, undefined, () =>
    bootState({ visible: !document.hidden, route: routeOf(location) }),
  );
  const [frameVersion, frameArrived] = useReducer(
    (version: number) => version + 1,
    0,
  );
  const painted = useRef(false);
  const arrivals = useRef<{ key: SpaceKey; at: number }[]>([]);
  const paintCallbacks = useRef(new Set<number>());
  const selectedForPaint = useRef<SpaceKey | null>(null);
  const requests = useRef(new Map<number, AbortController>());
  const lastUpdate = useRef<number | null>(null);
  const main = useRef<HTMLElement>(null);
  const [evidence, focusEvidence] = useReducer(evidenceReduce, {
    kind: "closed",
    version: 0,
  });
  useEffect(() => {
    if (evidence.kind !== "loading") return;
    const controller = new AbortController();
    const { request, version } = evidence;
    fetch(
      `/w/${request.key}/messages/${encodeURIComponent(request.target.messageId)}/context`,
      {
        credentials: "same-origin",
        signal: controller.signal,
      },
    )
      .then(async (response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const context = parseMessageContext(
          await response.json(),
          request.target,
        );
        if (!controller.signal.aborted)
          focusEvidence({ type: "loaded", version, context });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted)
          focusEvidence({
            type: "failed",
            version,
            reason: error instanceof Error ? error.message : "Request failed",
          });
      });
    return () => controller.abort();
  }, [evidence]);
  useEffect(() => {
    if (
      evidence.kind !== "closed" &&
      evidence.request.key !== state.selection.key
    )
      focusEvidence({ type: "close" });
  }, [evidence, state.selection.key]);
  const [poster] = useState(createPoster);
  const [reader] = useState(createScreenReader);
  const inFlightPosts = useRef(new Map<string, ComposerId>());
  const posted = useRef(new Set<string>());
  useEffect(() => poster.prepare(), [poster]);
  useEffect(() => {
    const active = new Set<number>();
    for (const channel of state.channels.values()) {
      if (channel.history.kind !== "loading") continue;
      const { request } = channel.history;
      active.add(request.id);
      if (requests.current.has(request.id)) continue;
      const controller = new AbortController();
      requests.current.set(request.id, controller);
      const params = new URLSearchParams({
        before: request.before,
        limit: "50",
      });
      fetch(`/w/${request.key}/messages?${params}`, {
        signal: controller.signal,
        credentials: "same-origin",
      })
        .then(async (response) => {
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const messages = parseHistory(await response.json());
          dispatch({
            type: "history",
            request,
            result: { kind: "page", messages },
          });
        })
        .catch((error: unknown) => {
          if (!controller.signal.aborted)
            dispatch({
              type: "history",
              request,
              result: {
                kind: "error",
                reason:
                  error instanceof Error ? error.message : "Request failed",
              },
            });
        });
    }
    for (const [id, controller] of requests.current)
      if (!active.has(id)) {
        controller.abort();
        requests.current.delete(id);
      }
    for (const channel of state.channels.values())
      if (channel.runs.length > 1 && channel.history.kind === "idle")
        dispatch({ type: "older", key: channel.key });
  }, [state.channels]);
  useEffect(() => {
    const controllers = requests.current;
    return () => {
      for (const controller of controllers.values()) controller.abort();
    };
  }, []);
  useLayoutEffect(() => {
    selectedForPaint.current = state.selection.key;
    if (
      frameVersion === 0 ||
      arrivals.current.length === 0 ||
      state.sync.kind === "replaying"
    )
      return;
    const key = state.selection.key;
    const starts = arrivals.current
      .splice(0)
      .filter(
        (arrival) =>
          arrival.key === key && state.channels.get(arrival.key)?.messages.size,
      );
    if (starts.length === 0) return;
    const callbacks = paintCallbacks.current;
    const first = requestAnimationFrame(() => {
      callbacks.delete(first);
      const second = requestAnimationFrame(() => {
        callbacks.delete(second);
        if (selectedForPaint.current !== key) return;
        const name = "namedrop:frame-to-message-paint";
        if (
          performance.getEntriesByName(name, "measure").length + starts.length >
          200
        )
          performance.clearMeasures(name);
        const end = performance.now();
        for (const { at: start } of starts)
          performance.measure(name, { start, end });
      });
      callbacks.add(second);
    });
    callbacks.add(first);
    if (callbacks.size > 200) {
      const oldest = callbacks.values().next().value;
      if (oldest !== undefined) {
        cancelAnimationFrame(oldest);
        callbacks.delete(oldest);
      }
    }
  }, [state, frameVersion]);
  useEffect(() => {
    const callbacks = paintCallbacks.current;
    return () => {
      for (const callback of callbacks) cancelAnimationFrame(callback);
    };
  }, []);
  useEffect(() => {
    let stream: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let replayTimer: ReturnType<typeof setTimeout> | undefined;
    let generation = 0;
    function cancelReplay() {
      clearTimeout(replayTimer);
      replayTimer = undefined;
      generation += 1;
    }
    function connect() {
      if (disposed) return;
      cancelReplay();
      const source = new EventSource("/api/events");
      stream = source;
      source.addEventListener("open", () => {
        if (disposed || stream !== source) return;
        cancelReplay();
        arrivals.current = [];
        dispatch({ type: "opened" });
        const openedGeneration = generation;
        replayTimer = setTimeout(() => {
          if (disposed || stream !== source || generation !== openedGeneration)
            return;
          replayTimer = undefined;
          dispatch({ type: "replay-timeout" });
        }, 2000);
      });
      for (const name of ["space", "gone", "ready"]) {
        source.addEventListener(name, (message) => {
          if (
            disposed ||
            stream !== source ||
            !(message instanceof MessageEvent) ||
            typeof message.data !== "string"
          )
            return;
          const arrivedAt = performance.now();
          const event = parseEvent(name, message.data);
          if (event) {
            if (event.type === "space") {
              lastUpdate.current = Date.now();
              arrivals.current = [
                ...arrivals.current,
                { key: event.channel.key, at: arrivedAt },
              ].slice(-200);
              frameArrived();
            }
            if (event.type === "ready") cancelReplay();
            dispatch(event);
          }
        });
      }
      const disconnected = () => {
        if (disposed || stream !== source) return;
        cancelReplay();
        arrivals.current = [];
        dispatch({ type: "disconnected" });
        if (source.readyState === EventSource.CLOSED && retry === undefined) {
          source.close();
          retry = setTimeout(() => {
            retry = undefined;
            connect();
          }, 1000);
        }
      };
      source.addEventListener("error", disconnected);
      source.addEventListener("restarting", disconnected);
    }
    const visibility = () =>
      dispatch({ type: "visibility", visible: !document.hidden });
    document.addEventListener("visibilitychange", visibility);
    connect();
    return () => {
      disposed = true;
      cancelReplay();
      stream?.close();
      clearTimeout(retry);
      document.removeEventListener("visibilitychange", visibility);
    };
  }, []);
  useEffect(() => {
    if (painted.current || state.channels.size === 0) return;
    let second = 0;
    const first = requestAnimationFrame(() => {
      second = requestAnimationFrame(() => {
        performance.mark("namedrop:sidebar-painted");
        performance.measure("namedrop:load-to-sidebar-paint", {
          start: 0,
          end: "namedrop:sidebar-painted",
        });
        performance.clearMarks("namedrop:sidebar-painted");
        painted.current = true;
      });
    });
    return () => {
      cancelAnimationFrame(first);
      cancelAnimationFrame(second);
    };
  }, [state.channels.size]);
  useEffect(() => {
    const back = () => dispatch(routeEvent(routeOf(location)));
    addEventListener("popstate", back);
    return () => removeEventListener("popstate", back);
  }, []);
  const discovering =
    state.landing.kind === "discovering" &&
    (state.channels.size > 0 ||
      (state.sync.kind === "replaying" && state.sync.pending.size > 0));
  useEffect(() => {
    if (!discovering) return;
    const timer = setTimeout(
      () => dispatch({ type: "discovery-timeout" }),
      2000,
    );
    return () => clearTimeout(timer);
  }, [discovering]);
  useEffect(() => {
    const names = ["pointerdown", "keydown", "wheel", "touchstart"] as const;
    const input = () => {
      dispatch({ type: "input" });
      for (const name of names) removeEventListener(name, input, true);
    };
    for (const name of names)
      addEventListener(name, input, { capture: true, passive: true });
    return () => {
      for (const name of names) removeEventListener(name, input, true);
    };
  }, []);
  useEffect(() => {
    const key = state.selection.key;
    if (key === null || !state.visible) return;
    const dwell = setTimeout(() => dispatch({ type: "read", key }), 1500);
    return () => clearTimeout(dwell);
  }, [state.selection.key, state.visible]);
  let needsYou = 0;
  for (const channel of state.channels.values())
    if (channel.health.level === "needs_you") needsYou++;
  useEffect(() => {
    document.title =
      needsYou > 0 ? `(${needsYou}) Needs you · Namedrop` : "Namedrop";
  }, [needsYou]);
  const send = useCallback(
    (
      outgoing: Pick<Outgoing, "composer" | "source" | "inReplyTo">,
      startedAt: number,
    ) => {
      // A second Enter can land before the render that locks the box.
      for (const composer of inFlightPosts.current.values())
        if (sameComposer(composer, outgoing.composer)) return;
      const tried = state.drafts.get(draftKey(outgoing.composer))?.attempt;
      const requestId =
        tried?.source === outgoing.source &&
        tried.inReplyTo === outgoing.inReplyTo
          ? tried.requestId
          : crypto.randomUUID();
      inFlightPosts.current.set(requestId, outgoing.composer);
      dispatch({
        type: "send",
        outgoing: { ...outgoing, requestId, at: Date.now() },
      });
      void poster
        .post({
          requestId,
          key: outgoing.composer.key,
          source: outgoing.source,
          inReplyTo: outgoing.inReplyTo,
        })
        .then((result) => {
          inFlightPosts.current.delete(requestId);
          if (result.kind === "failed") {
            dispatch({
              type: "send-failed",
              requestId,
              status: result.status,
              message: result.message,
            });
            return;
          }
          measure("namedrop:enter-to-sent", startedAt);
          posted.current.add(result.messageId);
          const [oldest] = posted.current;
          if (posted.current.size > POSTED_LIMIT && oldest !== undefined)
            posted.current.delete(oldest);
          dispatch({ type: "sent", requestId, messageId: result.messageId });
          focusEvidence({
            type: "sent",
            key: outgoing.composer.key,
            messageId: outgoing.inReplyTo,
          });
        });
    },
    [poster, state.drafts],
  );
  const edit = useCallback(
    (composer: ComposerId, text: string) =>
      dispatch({ type: "edit", composer, text }),
    [],
  );
  const select = useCallback((key: SpaceKey) => {
    focusEvidence({ type: "close" });
    go({ kind: "channel", key });
    dispatch({ type: "select", key });
  }, []);
  const showActivity = useCallback(() => {
    focusEvidence({ type: "close" });
    go({ kind: "activity" });
    dispatch({ type: "activity" });
  }, []);
  const jump = useCallback((key: SpaceKey) => {
    focusEvidence({ type: "close" });
    go({ kind: "channel", key });
    dispatch({ type: "jump", key });
  }, []);
  const connection = connectionStatus(state);
  const channel =
    state.selection.key === null
      ? undefined
      : state.channels.get(state.selection.key);
  const messages = channel?.messages;
  const windows = channel?.runs;
  const threads = channel?.threads;
  const members = channel?.members ?? noMembers;
  const { groups, rows } = useMemo(() => {
    if (!messages || !windows || !threads)
      return { groups: noGroups, rows: noRows };
    const window = { messages, runs: windows, threads, members };
    const groups = threadGroups(window);
    const rows = timelineView(window, groups);
    return { groups, rows };
  }, [messages, windows, threads, members]);
  const homesSeen = useRef<ReadonlyMap<string, Home> | undefined>(undefined);
  const homes = useMemo(() => {
    homesSeen.current = homesOf(state.channels.values(), homesSeen.current);
    return homesSeen.current;
  }, [state.channels]);
  const activitySeen = useRef<ActivityList | undefined>(undefined);
  const activity = useMemo(() => {
    const start = performance.now();
    activitySeen.current = activityOf(state.channels, activitySeen.current);
    measure("namedrop:activity-derive", start);
    return activitySeen.current;
  }, [state.channels]);
  const channelRule = useMemo<SendRule>(
    () => ({ kind: "channel", agents: members }),
    [members],
  );
  const directory = useMemo<Directory>(
    () => ({ members, homes }),
    [members, homes],
  );
  const current = channel?.key;
  const allChannels = state.channels;
  const completion = useMemo<CompletionContext | null>(
    () =>
      current === undefined
        ? null
        : {
            current,
            channels: channelOptions(allChannels.values()),
            membersOf: (key) => [
              ...(allChannels.get(key)?.members.values() ?? []),
            ],
          },
    [current, allChannels],
  );
  const composerId = useMemo<ComposerId | null>(
    () => (current === undefined ? null : { key: current, thread: null }),
    [current],
  );
  const pending = useMemo(
    () =>
      composerId === null
        ? noPending
        : [...state.outbox.values()].filter((entry) =>
            sameComposer(entry.composer, composerId),
          ),
    [composerId, state.outbox],
  );
  const unsentReplies = useMemo(() => {
    const threads = new Set<string>();
    for (const draft of state.drafts.values())
      if (draft.key === current && draft.thread !== null && draft.failure)
        threads.add(draft.thread);
    return threads;
  }, [current, state.drafts]);
  const terminalRuns = channel?.terminalRuns;
  const runs = useMemo(() => runsByMessage(terminalRuns ?? []), [terminalRuns]);
  const focused =
    evidence.kind !== "closed" && evidence.request.key === channel?.key
      ? evidence
      : null;
  const thread = useMemo(
    () =>
      focused?.kind === "ready" && channel !== undefined
        ? evidenceThread(focused.context, channel.messages, channel.members)
        : state.slot.kind === "thread"
          ? groups.get(state.slot.id)
          : undefined,
    [focused, channel, state.slot, groups],
  );
  const channelResponse =
    focused?.kind === "ready" && focused.request.response?.kind === "message"
      ? focused.request.response
      : null;
  useLayoutEffect(() => {
    if (
      focused?.kind !== "ready" ||
      channelResponse?.preparation !== "requested" ||
      composerId === null
    )
      return;
    const result = prepareChannelMessage(
      channelResponse,
      directory,
      state.drafts.get(draftKey(composerId))?.text ?? "",
      pending.length > 0,
    );
    focusEvidence({
      type: "message-prepared",
      version: focused.version,
      result: result.kind,
    });
    if (result.kind === "prepared") {
      edit(composerId, result.text);
      main.current?.querySelector("textarea")?.focus();
    }
  }, [
    focused,
    channelResponse,
    composerId,
    directory,
    state.drafts,
    pending,
    edit,
  ]);
  const peeking = state.slot.kind === "peek" ? state.slot.target : null;
  const who = peeking === null ? null : peekWho(state.channels, peeking);
  const pane = thread || focused ? "thread" : who ? "peek" : null;
  const paneId = useId();
  const returnFocus = useRef<{
    button: HTMLButtonElement;
    threadId: string;
  } | null>(null);
  // Where focus goes once an opened pane renders.
  const focusIntent = useRef<"pane" | "composer" | null>(null);
  const [, requestFocus] = useReducer((count: number) => count + 1, 0);
  const openThread = useCallback(
    (
      threadId: string,
      button: HTMLButtonElement,
      focus: "pane" | "composer" = "pane",
    ) => {
      focusEvidence({ type: "close" });
      returnFocus.current = { button, threadId };
      focusIntent.current = focus;
      requestFocus();
      dispatch({ type: "thread", threadId });
    },
    [],
  );
  const sendTo = useCallback(
    (post: Message, alias: string, button: HTMLButtonElement) => {
      if (current === undefined) return;
      // The trailing space keeps the caret off a mention the post ends with,
      // so the member list stays closed and Enter sends.
      dispatch({
        type: "fill",
        composer: { key: current, thread: post.threadId },
        text: `${alias} ${post.body.text} `,
      });
      // A loaded conversation pane already shows this thread, and the timeline may not hold it.
      if (
        focused?.kind === "ready" &&
        focused.context.threadId === post.threadId
      ) {
        focusIntent.current = "composer";
        requestFocus();
        return;
      }
      openThread(post.threadId, button, "composer");
    },
    [current, focused, openThread],
  );
  const [located, locate] = useState<Located | null>(null);
  useEffect(() => {
    if (located === null) return;
    const done = setTimeout(() => locate(null), 1500);
    return () => clearTimeout(done);
  }, [located]);
  const showAttention = useCallback<FocusAttention>(
    (key, navigation, button) => {
      const { target } = navigation;
      go({ kind: "channel", key });
      dispatch({ type: "jump", key });
      if (navigation.kind === "loaded") {
        openThread(target.threadId, button);
      } else {
        returnFocus.current = { button, threadId: target.threadId };
        focusIntent.current = null;
        dispatch({ type: "thread", threadId: target.threadId });
        focusEvidence({
          type: "open",
          request: { key, target, response: navigation.response },
        });
      }
      locate({ messageId: target.messageId });
    },
    [openThread],
  );
  const closeThread = useCallback(() => {
    focusEvidence({ type: "close" });
    dispatch({ type: "thread", threadId: null });
    const saved = returnFocus.current;
    if (!saved) return;
    const button = saved.button.isConnected
      ? saved.button
      : [
          ...document.querySelectorAll<HTMLButtonElement>(
            ".timeline button[data-thread-id]",
          ),
        ].find((button) => button.dataset.threadId === saved.threadId);
    button?.focus();
  }, []);
  const opening = useRef<{
    element: HTMLElement;
    key: PeekKey;
    startedAt: number;
  } | null>(null);
  const openPeek = useCallback(
    (target: PeekTarget, element: HTMLElement, startedAt: number) => {
      focusEvidence({ type: "close" });
      opening.current = { element, key: peekKey(target), startedAt };
      focusIntent.current = "pane";
      requestFocus();
      dispatch({ type: "peek", target });
    },
    [],
  );
  const closePeek = useCallback(() => {
    dispatch({ type: "peek", target: null });
    const saved = opening.current;
    const control =
      saved === null
        ? null
        : saved.element.isConnected
          ? saved.element
          : document.querySelector<HTMLElement>(
              `main [data-peek="${CSS.escape(saved.key)}"]`,
            );
    (control ?? document.querySelector<HTMLElement>(MAIN_SCROLLER))?.focus();
  }, []);
  const scope = channel?.key ?? null;
  const peekAccess = useMemo<PeekAccess>(
    () => ({ scope, open: openPeek }),
    [scope, openPeek],
  );
  const paneWasOpen = useRef(false);
  const view = viewOf(state.selection);
  const shownView = useRef(view);
  useLayoutEffect(() => {
    if (pane !== null && focusIntent.current !== null) {
      const opened = document.getElementById(paneId);
      const box = opened?.querySelector("textarea");
      if (pane === "thread" && focusIntent.current === "composer" && box) {
        box.setSelectionRange(box.value.length, box.value.length);
        box.focus();
      } else
        opened
          ?.querySelector<HTMLElement>(
            pane === "thread"
              ? 'button[aria-label="Close thread"]'
              : ".peek-text",
          )
          ?.focus();
      focusIntent.current = null;
    }
    const unmounted =
      (pane === null && paneWasOpen.current) ||
      (view !== "activity" && shownView.current === "activity");
    if (unmounted && document.activeElement === document.body)
      document.querySelector<HTMLElement>(MAIN_SCROLLER)?.focus();
    paneWasOpen.current = pane !== null;
    shownView.current = view;
  });
  const prepareResponse = () =>
    focusEvidence({
      type: "prepare-message",
      draftEmpty:
        composerId !== null &&
        (state.drafts.get(draftKey(composerId))?.text.length ?? 0) === 0,
    });
  let threadResponse: ThreadResponse | null = null;
  if (focused?.kind === "ready") {
    const response = focused.request.response;
    switch (response?.kind) {
      case "reply":
        threadResponse = { kind: "reply", intent: response };
        break;
      case "message":
        threadResponse = {
          kind: "message",
          prepare: prepareResponse,
        };
        break;
      case "pane":
        threadResponse = {
          kind: "pane",
          name: members.get(response.addressId)?.alias ?? response.addressId,
        };
        break;
      case undefined:
        break;
      default: {
        const exhaustive: never = response;
        return exhaustive;
      }
    }
  }
  const updatedAt = lastUpdate.current;
  return (
    <PeekContext value={peekAccess}>
      <div
        className={`app-shell${pane === null ? "" : ` has-slot has-${pane}`}`}
      >
        <a className="skip-link" href="#main-content">
          Skip to messages
        </a>
        <Sidebar
          state={state}
          busy={activity.count}
          onSelect={select}
          onActivity={showActivity}
        />
        <main className="main" id="main-content" tabIndex={-1} ref={main}>
          {connection === "reconnecting" && (
            <div className="reconnect" role="status">
              <Icon name="refresh" />
              Reconnecting…
              {updatedAt !== null &&
                ` Showing updates up to ${new Date(updatedAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}.`}
            </div>
          )}
          {state.selection.kind === "activity" && state.channels.size > 0 ? (
            <ActivityView
              list={activity}
              homes={homes}
              open={peeking === null ? null : peekKey(peeking)}
              onJump={jump}
              channels={state.channels}
              onFocus={showAttention}
            />
          ) : channel ? (
            <>
              <ChannelHeader
                channel={channel}
                suffix={channelSuffix(channel, state.channels.values())}
              />
              <MemberStrip key={channel.key} members={channel.members} />
              <NeedsYou channel={channel} onFocus={showAttention} />
              <Timeline
                channel={channel}
                rows={rows}
                runs={runs}
                directory={directory}
                pending={pending}
                posted={posted.current}
                located={located}
                unsentReplies={unsentReplies}
                expandedThread={thread ? thread.id : null}
                onThread={openThread}
                onSendTo={sendTo}
                paneId={paneId}
                onOlder={(key) => dispatch({ type: "older", key })}
                onLatest={(key) => dispatch({ type: "latest", key })}
                onFollow={(key, atBottom) =>
                  dispatch({ type: "follow", key, atBottom })
                }
              />
              {channelResponse?.preparation === "preserved" && (
                <p className="earlier-thread" role="status">
                  Your channel draft or pending send was kept. Send or clear it,
                  then retry.{" "}
                  <button
                    type="button"
                    className="text-button"
                    onClick={prepareResponse}
                  >
                    Prepare response
                  </button>
                </p>
              )}
              {channelResponse?.preparation === "unavailable" && (
                <p className="earlier-thread" role="status">
                  The responsible agent is no longer available. Use its pane in
                  Herdr to respond.
                </p>
              )}
              {completion && composerId && (
                <Composer
                  key={`composer:${channel.key}`}
                  context={completion}
                  composer={composerId}
                  drafts={state.drafts}
                  outbox={state.outbox}
                  placeholder={composerPlaceholder(channel)}
                  label={composerPlaceholder(channel)}
                  rule={channelRule}
                  onEdit={(text) => edit(composerId, text)}
                  onSend={(source, startedAt) => {
                    if (channel.viewMode === "browsing")
                      dispatch({ type: "latest", key: channel.key });
                    send(
                      { composer: composerId, source, inReplyTo: null },
                      startedAt,
                    );
                  }}
                />
              )}
            </>
          ) : state.channels.size > 0 ? (
            <div className="empty">
              <h1>Choose a channel</h1>
              <p>Select a channel to read its messages and live status.</p>
            </div>
          ) : connection === "live" ? (
            <div className="empty">
              <Icon name="inbox" />
              <h1>No workspaces yet</h1>
              <p>
                Open a workspace in Herdr and name an agent with{" "}
                <code className="inline">namedrop name</code>. It shows up here
                as a channel.
              </p>
            </div>
          ) : (
            <>
              <h1 className="sr-only">
                {connection === "reconnecting"
                  ? "Waiting to reconnect"
                  : "Connecting to Namedrop"}
              </h1>
              <div className="skeleton" aria-hidden="true">
                <div className="skeleton-message">
                  <span />
                </div>
                <div className="skeleton-message">
                  <span />
                </div>
                <div className="skeleton-message">
                  <span />
                </div>
              </div>
            </>
          )}
        </main>
        {channel &&
          thread &&
          (focused === null || focused.kind === "ready") && (
            <ThreadPane
              key={
                focused === null ? thread.id : `${thread.id}:${focused.version}`
              }
              channel={channel}
              thread={thread}
              runs={runs}
              directory={directory}
              completion={completion}
              drafts={state.drafts}
              outbox={state.outbox}
              posted={posted.current}
              onEdit={edit}
              onSend={send}
              onSendTo={sendTo}
              onClose={closeThread}
              paneId={paneId}
              response={threadResponse}
              target={focused?.kind === "ready" ? focused.request.target : null}
              earlierOmitted={
                focused?.kind === "ready" && focused.context.earlierOmitted
              }
            />
          )}
        {channel && focused && focused.kind !== "ready" && (
          <aside className="slot thread" aria-label="Thread" id={paneId}>
            <header className="thread-head">
              <h2 className="thread-title">Thread</h2>
              <button
                type="button"
                className="icon-button"
                onClick={closeThread}
                aria-label="Close thread"
              >
                <Icon name="x" />
              </button>
            </header>
            <div className="thread-body" role="status">
              {focused.kind === "loading" ? (
                "Loading conversation…"
              ) : (
                <>
                  Could not load conversation. {focused.reason}{" "}
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => focusEvidence({ type: "retry" })}
                  >
                    Retry
                  </button>
                </>
              )}
            </div>
          </aside>
        )}
        {peeking && who && (
          <PeekDrawer
            target={peeking}
            who={who}
            reader={reader}
            startedAt={opening.current?.startedAt ?? 0}
            paneId={paneId}
            onClose={closePeek}
          />
        )}
      </div>
    </PeekContext>
  );
}
