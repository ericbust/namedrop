import { useEffect, useReducer, useRef } from "react";
import type { Event, State } from "../model.ts";
import {
  channelName,
  connectionStatus,
  initialState,
  reduce,
} from "../store.ts";
import { parseEvent } from "../wire.ts";
import { MemberList } from "./MemberList.tsx";
import { Sidebar } from "./Sidebar.tsx";

function measuredReduce(state: State, event: Event): State {
  if (event.type !== "space" && event.type !== "gone" && event.type !== "ready")
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
  const painted = useRef(false);
  useEffect(() => {
    let stream: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    function connect() {
      if (disposed) return;
      const source = new EventSource("/api/events");
      stream = source;
      source.addEventListener("open", () => dispatch({ type: "opened" }));
      for (const name of ["space", "gone", "ready"]) {
        source.addEventListener(name, (message) => {
          if (
            !(message instanceof MessageEvent) ||
            typeof message.data !== "string"
          )
            return;
          const event = parseEvent(name, message.data);
          if (event) dispatch(event);
        });
      }
      const disconnected = () => {
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
        Skip to members
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
          <MemberList
            channel={channel}
            name={channelName(channel, [...state.channels.values()])}
          />
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
                ? "Select a workspace to see its members and live status."
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
