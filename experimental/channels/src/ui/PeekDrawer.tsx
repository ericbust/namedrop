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
      <PeekScreen
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
}: Omit<Props, "paneId" | "inert"> & { retention: Retention }) {
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
              <span className="peek-presence">
                {presenceWord[who.activity]}
              </span>
            )}
          </p>
          <p className="peek-where" title={who.channel.suffix ?? undefined}>
            {peekCopy.where(who.channel.label)}
            {view.stamp !== null && " · "}
            <span aria-live="polite">{view.stamp}</span>
          </p>
        </div>
        <button
          type="button"
          className="peek-refresh"
          aria-disabled={view.refreshing || undefined}
          onClick={again}
        >
          <Icon name="refresh" />
          {peekCopy.refresh}
        </button>
        <button
          type="button"
          className="icon-button"
          onClick={onClose}
          aria-label={peekCopy.close}
        >
          <Icon name="x" />
        </button>
      </header>
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
