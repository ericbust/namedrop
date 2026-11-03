import {
  type ReactNode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
} from "react";
import { peekCopy, presenceWord } from "../copy.ts";
import type { LiveState } from "../livePeek.ts";
import type { PeekTarget } from "../model.ts";
import {
  BUSY_AFTER_MS,
  firstRead,
  type PeekKey,
  type PeekNotice,
  type PeekSnapshot,
  peekKey,
  peekReduce,
  peekView,
  SLOW_AFTER_MS,
} from "../peek.ts";
import type { ScreenReader } from "../screen.ts";
import type { PeekWho } from "../store.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";
import { measure } from "./measure.ts";
import { PeekCompose } from "./PeekCompose.tsx";
import { PeekLive } from "./PeekLive.tsx";
import { useEscape } from "./useEscape.ts";

type Retention = {
  id: number;
  snapshots: Map<PeekKey, PeekSnapshot>;
};
const retained = new WeakMap<ScreenReader, Retention>();
let nextRetentionId = 0;
const MAX_SCREENS = 8;
const MAX_CHARACTERS = 512 * 1024;

function retentionFor(reader: ScreenReader): Retention {
  const existing = retained.get(reader);
  if (existing) return existing;
  const retention: Retention = { id: ++nextRetentionId, snapshots: new Map() };
  retained.set(reader, retention);
  return retention;
}

function retainSnapshot(
  retention: Retention,
  key: PeekKey,
  snapshot: PeekSnapshot,
) {
  const { snapshots } = retention;
  snapshots.delete(key);
  if (snapshot.screen.text.length > MAX_CHARACTERS) return;
  snapshots.set(key, snapshot);
  let characters = 0;
  for (const { screen } of snapshots.values()) characters += screen.text.length;
  for (const [oldest, { screen }] of snapshots) {
    if (snapshots.size <= MAX_SCREENS && characters <= MAX_CHARACTERS) break;
    snapshots.delete(oldest);
    characters -= screen.text.length;
  }
}

type Props = {
  target: PeekTarget;
  who: PeekWho;
  reader: ScreenReader;
  startedAt: number;
  paneId: string;
  toggle: ReactNode;
  inert: boolean;
  onClose: () => void;
};
export function PeekDrawer({
  target,
  who,
  reader,
  paneId,
  inert,
  onClose,
  ...rest
}: Props) {
  const retention = retentionFor(reader);
  const pane = useRef<HTMLElement>(null);
  useEscape(pane, onClose);
  return (
    <aside
      className="slot peek"
      aria-label={peekCopy.label(who.alias)}
      id={paneId}
      ref={pane}
      inert={inert}
    >
      <PeekContent
        key={`${retention.id}/${peekKey(target)}`}
        target={target}
        who={who}
        onClose={onClose}
        reader={reader}
        retention={retention}
        {...rest}
      />
    </aside>
  );
}

type ContentProps = Omit<Props, "paneId" | "inert"> & { retention: Retention };
function liveWords(state: LiveState): {
  stamp: string | null;
  notice: string | null;
} {
  switch (state.kind) {
    case "connecting":
      return { stamp: peekCopy.liveConnecting, notice: null };
    case "reconnecting":
      return { stamp: peekCopy.liveReconnecting, notice: null };
    case "live":
      return { stamp: peekCopy.live, notice: null };
    case "session-ended":
      return { stamp: null, notice: peekCopy.liveSession };
    case "unavailable":
      return { stamp: null, notice: peekCopy.liveUnavailable };
    case "closed":
      return {
        stamp: null,
        notice: {
          ended: peekCopy.liveEnded,
          replaced: peekCopy.liveReplaced,
          missing: peekCopy.liveMissing,
        }[state.reason],
      };
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}
function PeekContent(props: ContentProps) {
  const [mode, setMode] = useState<"live" | "text">("live");
  const [fallback, setFallback] = useState(false);
  const [state, setState] = useState<LiveState>({ kind: "connecting" });
  const [attempt, setAttempt] = useState(0);
  const focusScreen = useRef(false);
  useLayoutEffect(() => {
    if (focusScreen.current) {
      document.querySelector<HTMLElement>(".peek .peek-text")?.focus();
      focusScreen.current = false;
    }
  });
  const { retention, target } = props;
  const update = useCallback(
    (next: LiveState) => {
      setState(next);
      if (next.kind === "session-ended") retention.snapshots.clear();
      if (next.kind === "closed" && next.reason !== "ended")
        retention.snapshots.delete(peekKey(target));
      if (next.kind === "unavailable" && next.reason === "unsupported") {
        focusScreen.current = Boolean(
          document.activeElement?.closest(".peek-text"),
        );
        setFallback(true);
        setMode("text");
      }
    },
    [retention, target],
  );
  const controls = (
    <fieldset className="peek-modes" aria-label={peekCopy.view}>
      <button
        type="button"
        aria-pressed={mode === "live"}
        onClick={() => {
          if (mode !== "live") setState({ kind: "connecting" });
          setMode("live");
          setFallback(false);
        }}
      >
        {peekCopy.live}
      </button>
      <button
        type="button"
        aria-pressed={mode === "text"}
        onClick={() => {
          setMode("text");
          setFallback(false);
        }}
      >
        {peekCopy.snapshot}
      </button>
    </fieldset>
  );
  if (mode === "text")
    return (
      <PeekScreen
        {...props}
        controls={
          <>
            {controls}
            {fallback && (
              <p className="peek-fallback">{peekCopy.liveUnsupported}</p>
            )}
          </>
        }
      />
    );
  const { stamp, notice } = liveWords(state);
  return (
    <>
      <PeekHeader
        who={props.who}
        toggle={props.toggle}
        onClose={props.onClose}
        stamp={stamp}
        action={
          state.kind === "unavailable" ? (
            <button
              type="button"
              className="peek-refresh"
              onClick={() => {
                focusScreen.current = true;
                setState({ kind: "connecting" });
                setAttempt((n) => n + 1);
              }}
            >
              {peekCopy.tryAgain}
            </button>
          ) : (
            <span className="peek-head-space" />
          )
        }
      />
      {controls}
      {notice !== null && (
        <p className="peek-note" role="status">
          {notice}
        </p>
      )}
      <PeekLive key={attempt} target={target} onState={update} />
      <PeekCompose
        key={state.kind === "live" ? state.continuity : state.kind}
        target={target}
        continuity={state.kind === "live" ? state.continuity : null}
      />
    </>
  );
}

function PeekHeader({
  who,
  toggle,
  onClose,
  stamp,
  action,
}: {
  who: PeekWho;
  toggle: ReactNode;
  onClose(): void;
  stamp: string | null;
  action: ReactNode;
}) {
  return (
    <header className="peek-head">
      {toggle}
      <Avatar
        alias={who.alias}
        type={who.kind}
        small
        presence={who.activity ?? undefined}
      />
      <div className="peek-who">
        <p className="peek-title">
          <span className="peek-alias">{who.alias}</span>
          {who.activity !== null && (
            <span className="peek-presence">{presenceWord[who.activity]}</span>
          )}
        </p>
        <p className="peek-where" title={who.channel.suffix ?? undefined}>
          {peekCopy.where(who.channel.label)}
          {stamp !== null && " · "}
          <span aria-live="polite">{stamp}</span>
        </p>
      </div>
      {action}
      <button
        type="button"
        className="icon-button"
        onClick={onClose}
        aria-label={peekCopy.close}
      >
        <Icon name="x" />
      </button>
    </header>
  );
}

function usePeekRead(
  target: PeekTarget,
  reader: ScreenReader,
  retention: Retention,
) {
  const key = peekKey(target);
  const [read, send] = useReducer(
    peekReduce,
    retention.snapshots.get(key) ?? null,
    firstRead,
  );
  const request = read.phase === "reading" ? read.request : null;
  useEffect(() => {
    if (request === null) return;
    const controller = new AbortController();
    const slow = setTimeout(
      () => send({ type: "waited", request, wait: "slow" }),
      SLOW_AFTER_MS,
    );
    const busy = setTimeout(
      () => send({ type: "waited", request, wait: "busy" }),
      BUSY_AFTER_MS,
    );
    void reader(target, controller.signal).then((result) => {
      if (controller.signal.aborted) return;
      const at = Date.now();
      if (result.kind === "text") {
        retainSnapshot(retention, key, { screen: result.screen, at });
      } else if (result.failure === "session") {
        retention.snapshots.clear();
      } else if (result.failure === "missing" || result.failure === "closed") {
        retention.snapshots.delete(key);
      }
      send({ type: "settled", request, result, at });
    });
    return () => {
      controller.abort();
      clearTimeout(slow);
      clearTimeout(busy);
    };
  }, [request, target, reader, retention, key]);
  const again = useCallback(() => send({ type: "again" }), []);
  return { read, again };
}

function Notice({
  notice,
  again,
  className,
}: {
  notice: PeekNotice;
  again: () => void;
  className: string;
}) {
  return (
    <div className={className} role="status">
      <p className="peek-notice-title">{notice.title}</p>
      <p className="peek-notice-body" title={notice.body}>
        {notice.body}
      </p>
      {notice.retry && (
        <button
          type="button"
          className="text-button peek-retry"
          onClick={again}
        >
          {peekCopy.tryAgain}
        </button>
      )}
    </div>
  );
}

function PeekScreen({
  target,
  who,
  reader,
  startedAt,
  onClose,
  retention,
  toggle,
  controls,
}: ContentProps & { controls: ReactNode }) {
  const { read, again } = usePeekRead(target, reader, retention);
  const view = peekView(read, who);
  const { body } = view;
  const screen = useRef<HTMLElement>(null);
  const atBottom = useRef(true);
  const [openedAt] = useState(startedAt);
  const marked = useRef(false);
  const text = body.kind === "text" ? body.text : null;
  useLayoutEffect(() => {
    const element = screen.current;
    if (text !== null && element && atBottom.current)
      element.scrollTop = element.scrollHeight;
  }, [text]);
  const painted = text !== null;
  useEffect(() => {
    if (!painted || marked.current) return;
    marked.current = true;
    let second = 0;
    const first = requestAnimationFrame(() => {
      second = requestAnimationFrame(() =>
        measure("namedrop:click-to-peek-text", openedAt),
      );
    });
    return () => {
      cancelAnimationFrame(first);
      cancelAnimationFrame(second);
    };
  }, [painted, openedAt]);
  return (
    <>
      <PeekHeader
        who={who}
        toggle={toggle}
        onClose={onClose}
        stamp={view.stamp}
        action={
          <button
            type="button"
            className="peek-refresh"
            aria-disabled={view.refreshing || undefined}
            onClick={again}
          >
            <Icon name="refresh" />
            {peekCopy.refresh}
          </button>
        }
      />
      {controls}
      {view.bar !== null && (
        <Notice notice={view.bar} again={again} className="peek-bar" />
      )}
      {body.kind === "reading" && (
        <div className="peek-note">
          <p>{peekCopy.readingScreen(who.alias)}</p>
          {body.busy && <p>{peekCopy.busy}</p>}
        </div>
      )}
      {body.kind === "empty" && <p className="peek-note">{peekCopy.empty}</p>}
      {body.kind === "failure" && (
        <Notice notice={body.notice} again={again} className="peek-failure" />
      )}
      <section
        className={`peek-text${text === null ? "" : " has-text"}`}
        aria-label={peekCopy.text}
        // biome-ignore lint/a11y/noNoninteractiveTabindex: Keyboard scrolling requires focus. https://www.w3.org/WAI/standards-guidelines/act/rules/0ssw9k/
        tabIndex={0}
        ref={screen}
        onScroll={(event) => {
          const element = event.currentTarget;
          atBottom.current =
            element.scrollHeight - element.scrollTop - element.clientHeight <=
            4;
        }}
      >
        {body.kind === "text" && body.earlierOmitted && (
          <p className="peek-omitted">{peekCopy.omitted}</p>
        )}
        {text !== null && <ScreenLines text={text} />}
      </section>
    </>
  );
}

// Each line is its own block so a wrapped line hangs under its start. The
// newlines stay in the text, so copying and textContent read like the screen.
function ScreenLines({ text }: { text: string }) {
  const lines = text.split("\n");
  return (
    <pre>
      {lines.map((line, index) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: A read's lines never reorder; the whole block re-renders per read.
        <span key={index} className="screen-line">
          {index < lines.length - 1 ? `${line}\n` : line}
        </span>
      ))}
    </pre>
  );
}
