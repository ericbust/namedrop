import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
} from "react";
import type { Event, SpaceKey, State } from "../model.ts";
import {
  channelSuffix,
  connectionStatus,
  initialState,
  reduce,
  runsByMessage,
  type ThreadGroup,
  threadGroups,
} from "../store.ts";
import { parseEvent, parseHistory, pathKey } from "../wire.ts";
import { ChannelHeader } from "./ChannelHeader.tsx";
import { Icon } from "./Icons.tsx";
import { MemberStrip } from "./MemberStrip.tsx";
import { NeedsYou } from "./NeedsYou.tsx";
import { Sidebar } from "./Sidebar.tsx";
import { ThreadPane } from "./ThreadPane.tsx";
import { Timeline } from "./Timeline.tsx";

const noGroups: ReadonlyMap<string, ThreadGroup> = new Map();
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
    initialState({
      visible: !document.hidden,
      requestedKey: pathKey(location.pathname),
    }),
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
    const back = () => {
      const key = pathKey(location.pathname);
      if (key !== null) dispatch({ type: "select", key });
    };
    addEventListener("popstate", back);
    return () => removeEventListener("popstate", back);
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
  const select = useCallback((key: SpaceKey) => {
    const path = `/w/${key}`;
    if (location.pathname !== path) history.pushState(null, "", path);
    dispatch({ type: "select", key });
  }, []);
  const connection = connectionStatus(state);
  const channel =
    state.selection.key === null
      ? undefined
      : state.channels.get(state.selection.key);
  const messages = channel?.messages;
  const windows = channel?.runs;
  const threads = channel?.threads;
  const groups = useMemo(
    () =>
      messages && windows && threads
        ? threadGroups({ messages, runs: windows, threads })
        : noGroups,
    [messages, windows, threads],
  );
  const terminalRuns = channel?.terminalRuns;
  const runs = useMemo(() => runsByMessage(terminalRuns ?? []), [terminalRuns]);
  const thread =
    state.openThread === null ? undefined : groups.get(state.openThread);
  const paneId = useId();
  const returnFocus = useRef<{
    button: HTMLButtonElement;
    threadId: string;
  } | null>(null);
  const focusIntent = useRef(false);
  const [, requestFocus] = useReducer((count: number) => count + 1, 0);
  const openThread = useCallback(
    (threadId: string, button: HTMLButtonElement) => {
      returnFocus.current = { button, threadId };
      focusIntent.current = true;
      requestFocus();
      dispatch({ type: "thread", threadId });
    },
    [],
  );
  const closeThread = useCallback(() => {
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
  useLayoutEffect(() => {
    if (thread && focusIntent.current) {
      document
        .getElementById(paneId)
        ?.querySelector<HTMLButtonElement>('button[aria-label="Close thread"]')
        ?.focus();
      focusIntent.current = false;
    }
  });
  const updatedAt = lastUpdate.current;
  return (
    <div className={`app-shell${channel && thread ? " has-thread" : ""}`}>
      <a className="skip-link" href="#main-content">
        Skip to messages
      </a>
      <Sidebar state={state} onSelect={select} />
      <main className="main" id="main-content" tabIndex={-1}>
        {connection === "reconnecting" && (
          <div className="reconnect" role="status">
            <Icon name="refresh" />
            Reconnecting…
            {updatedAt !== null &&
              ` Showing updates up to ${new Date(updatedAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}.`}
          </div>
        )}
        {channel ? (
          <>
            <ChannelHeader
              channel={channel}
              suffix={channelSuffix(channel, state.channels.values())}
            />
            <MemberStrip key={channel.key} members={channel.members} />
            <NeedsYou channel={channel} />
            <Timeline
              channel={channel}
              groups={groups}
              runs={runs}
              expandedThread={thread ? thread.id : null}
              onThread={openThread}
              paneId={paneId}
              onOlder={(key) => dispatch({ type: "older", key })}
              onLatest={(key) => dispatch({ type: "latest", key })}
              onFollow={(key, atBottom) =>
                dispatch({ type: "follow", key, atBottom })
              }
            />
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
              <code className="inline">namedrop name</code>. It shows up here as
              a channel.
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
      {channel && thread && (
        <ThreadPane
          key={thread.id}
          channel={channel}
          thread={thread}
          runs={runs}
          onClose={closeThread}
          paneId={paneId}
        />
      )}
    </div>
  );
}
