import { useEffect, useReducer, useRef } from "react";
import type { Event, State } from "../model.ts";
import {
  channelName,
  connectionStatus,
  initialState,
  reduce,
} from "../store.ts";
import { parseEvent, parseHistory } from "../wire.ts";
import { MemberList } from "./MemberList.tsx";
import { Sidebar } from "./Sidebar.tsx";
import { Timeline } from "./Timeline.tsx";

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
    initialState(!document.hidden),
  );
  const [frameVersion, frameArrived] = useReducer(
    (version: number) => version + 1,
    0,
  );
  const painted = useRef(false);
  const arrivals = useRef<number[]>([]);
  const requests = useRef(new Map<number, AbortController>());
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
  useEffect(() => {
    if (
      frameVersion === 0 ||
      arrivals.current.length === 0 ||
      state.channels.size === 0 ||
      state.sync.kind === "replaying"
    )
      return;
    const starts = arrivals.current.splice(0);
    let completed = false;
    let second = 0;
    const first = requestAnimationFrame(() => {
      second = requestAnimationFrame(() => {
        const name = "namedrop:frame-to-message-paint";
        if (
          performance.getEntriesByName(name, "measure").length + starts.length >
          200
        )
          performance.clearMeasures(name);
        const end = performance.now();
        for (const start of starts) performance.measure(name, { start, end });
        completed = true;
      });
    });
    return () => {
      cancelAnimationFrame(first);
      cancelAnimationFrame(second);
      if (!completed)
        arrivals.current = [...starts, ...arrivals.current].slice(-200);
    };
  }, [state, frameVersion]);
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
              arrivals.current = [...arrivals.current, arrivedAt].slice(-200);
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
  const connection = connectionStatus(state);
  const channel =
    state.selectedKey === null
      ? undefined
      : state.channels.get(state.selectedKey);
  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        Skip to messages
      </a>
      <Sidebar
        state={state}
        onSelect={(key) => dispatch({ type: "select", key })}
      />
      <main id="main-content" tabIndex={-1}>
        {connection === "reconnecting" && (
          <div className="reconnect-banner" role="status">
            Connection lost. Reconnecting automatically…
            <span>Showing the last received update.</span>
          </div>
        )}
        {channel ? (
          <>
            <MemberList
              channel={channel}
              name={channelName(channel, [...state.channels.values()])}
            />
            <Timeline
              channel={channel}
              onOlder={(key) => dispatch({ type: "older", key })}
            />
          </>
        ) : (
          <div className="empty-state">
            <h1>
              {state.channels.size > 0
                ? "Choose a channel"
                : connection === "live"
                  ? "No live workspaces"
                  : connection === "reconnecting"
                    ? "Waiting to reconnect"
                    : "Connecting to Namedrop"}
            </h1>
            <p>
              {state.channels.size > 0
                ? "Select a workspace to read its messages and live status."
                : connection === "live"
                  ? "Open a Herdr workspace to see its channel here."
                  : "Your workspaces will appear when the live connection is ready."}
            </p>
          </div>
        )}
      </main>
    </div>
  );
}
