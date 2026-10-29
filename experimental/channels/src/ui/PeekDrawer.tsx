import {
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
  type PeekNotice,
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

type Props = {
  target: PeekTarget;
  who: PeekWho;
  reader: ScreenReader;
  startedAt: number;
  paneId: string;
  onClose: () => void;
};
export function PeekDrawer({ target, who, paneId, onClose, ...rest }: Props) {
  const pane = useRef<HTMLElement>(null);
  useEscape(pane, onClose);
  return (
    <aside
      className="slot peek"
      aria-label={peekCopy.label(who.alias)}
      id={paneId}
      ref={pane}
    >
      <PeekScreen
        key={peekKey(target)}
        target={target}
        who={who}
        onClose={onClose}
        {...rest}
      />
    </aside>
  );
}

function usePeekRead(target: PeekTarget, reader: ScreenReader) {
  const [read, send] = useReducer(peekReduce, undefined, firstRead);
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
      if (!controller.signal.aborted)
        send({ type: "settled", request, result, at: Date.now() });
    });
    return () => {
      controller.abort();
      clearTimeout(slow);
      clearTimeout(busy);
    };
  }, [request, target, reader]);
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
}: Omit<Props, "paneId">) {
  const { read, again } = usePeekRead(target, reader);
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
