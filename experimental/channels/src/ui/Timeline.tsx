import {
  useCallback,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { Channel, SpaceKey } from "../model.ts";
import { threadGroups, timelineView } from "../store.ts";
import { Message } from "./Message.tsx";
import { ThreadPane } from "./ThreadPane.tsx";

type Anchor = {
  ids: readonly { id: string; offset: number }[];
  bottom: boolean;
  top: number;
};
function capture(element: HTMLElement): Anchor {
  const top = element.getBoundingClientRect().top;
  const ids = [...element.querySelectorAll<HTMLElement>("[data-message-id]")]
    .filter((row) => row.getBoundingClientRect().bottom > top)
    .slice(0, 4)
    .flatMap((row) =>
      row.dataset.messageId === undefined
        ? []
        : [
            {
              id: row.dataset.messageId,
              offset: row.getBoundingClientRect().top - top,
            },
          ],
    );
  return {
    ids,
    bottom:
      element.scrollHeight - element.scrollTop - element.clientHeight < 24,
    top: element.scrollTop,
  };
}
export function Timeline({
  channel,
  onOlder,
  onLatest,
}: {
  channel: Channel;
  onOlder: (key: SpaceKey) => void;
  onLatest: (key: SpaceKey) => void;
}) {
  const scroller = useRef<HTMLElement>(null);
  const anchors = useRef(new Map<SpaceKey, Anchor>());
  const [open, setOpen] = useState<{ key: SpaceKey; threadId: string } | null>(
    null,
  );
  const returnFocus = useRef<{
    button: HTMLButtonElement;
    threadId: string;
  } | null>(null);
  const paneId = useId();
  const focusIntent = useRef(false);
  const { messages, runs, threads } = channel;
  const groups = useMemo(
    () => threadGroups({ messages, runs, threads }),
    [messages, runs, threads],
  );
  const rows = useMemo(
    () => timelineView({ messages, runs, threads }, groups),
    [messages, runs, threads, groups],
  );
  const thread =
    open?.key === channel.key ? groups.get(open.threadId) : undefined;
  const close = useCallback(() => {
    setOpen(null);
    const saved = returnFocus.current;
    if (!saved) return;
    const button = saved.button.isConnected
      ? saved.button
      : [
          ...(scroller.current?.querySelectorAll<HTMLButtonElement>(
            "button[data-thread-id]",
          ) ?? []),
        ].find((button) => button.dataset.threadId === saved.threadId);
    button?.focus();
  }, []);
  const openThread = useCallback(
    (threadId: string, button: HTMLButtonElement) => {
      returnFocus.current = { button, threadId };
      focusIntent.current = true;
      setOpen({ key: channel.key, threadId });
    },
    [channel.key],
  );
  useLayoutEffect(() => {
    if (thread && focusIntent.current) {
      document
        .getElementById(paneId)
        ?.querySelector<HTMLButtonElement>('button[aria-label="Close thread"]')
        ?.focus();
      focusIntent.current = false;
    }
  });
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const anchor = anchors.current.get(channel.key);
    if (!anchor || anchor.bottom) element.scrollTop = element.scrollHeight;
    else {
      const rows = [
        ...element.querySelectorAll<HTMLElement>("[data-message-id]"),
      ];
      const match = anchor.ids.flatMap((saved) => {
        const row = rows.find((row) => row.dataset.messageId === saved.id);
        return row ? [{ row, offset: saved.offset }] : [];
      })[0];
      if (match)
        element.scrollTop +=
          match.row.getBoundingClientRect().top -
          element.getBoundingClientRect().top -
          match.offset;
      else element.scrollTop = anchor.top;
    }
    anchors.current.set(channel.key, capture(element));
  }, [channel]);
  const history = channel.history;
  const gap = channel.runs.length > 1;
  return (
    <div className={`conversation${thread ? " has-thread" : ""}`}>
      <section className="timeline" aria-label="Channel messages">
        <section
          className="timeline-scroll"
          ref={scroller}
          aria-label="Message history"
          // biome-ignore lint/a11y/noNoninteractiveTabindex: Keyboard scrolling requires focus. https://www.w3.org/WAI/standards-guidelines/act/rules/0ssw9k/
          tabIndex={0}
          onScroll={(event) => {
            anchors.current.set(channel.key, capture(event.currentTarget));
            if (
              event.currentTarget.scrollTop < 40 &&
              history.kind === "idle" &&
              !channel.exhausted
            )
              onOlder(channel.key);
          }}
        >
          <div className="history-control">
            {channel.viewMode === "browsing" && (
              <button
                type="button"
                className="text-button"
                onClick={() => {
                  anchors.current.delete(channel.key);
                  onLatest(channel.key);
                }}
              >
                Jump to latest
              </button>
            )}
            {history.kind === "loading" ? (
              <span role="status">
                Loading{" "}
                {history.request.purpose === "bridge" ? "missing" : "older"}{" "}
                messages…
              </span>
            ) : history.kind === "error" ? (
              <>
                <span role="alert">
                  Could not load messages: {history.reason}
                </span>
                <button
                  type="button"
                  className="text-button"
                  onClick={() => onOlder(channel.key)}
                >
                  Retry
                </button>
              </>
            ) : channel.messages.size > 0 && (!channel.exhausted || gap) ? (
              <button
                type="button"
                className="text-button"
                onClick={() => onOlder(channel.key)}
              >
                Load {gap ? "missing" : "older"} messages
              </button>
            ) : channel.messages.size > 0 ? (
              <span>Beginning of channel</span>
            ) : null}
          </div>
          {channel.messages.size === 0 && (
            <div className="empty-messages">
              <h2>No messages yet</h2>
              <p>Agent exchanges will appear here as they happen.</p>
            </div>
          )}
          {rows.map((row) =>
            row.kind === "gap" ? (
              <div className="message-gap" key={`gap:${row.before}`}>
                <span>Messages missing between updates</span>
                <button
                  type="button"
                  className="text-button"
                  disabled={history.kind === "loading"}
                  onClick={() => onOlder(channel.key)}
                >
                  Load missing messages
                </button>
              </div>
            ) : (
              <div key={row.message.id}>
                {row.kind === "orphan" && (
                  <button
                    type="button"
                    className="text-button earlier-thread"
                    data-thread-id={row.thread.id}
                    onClick={(event) => {
                      openThread(row.thread.id, event.currentTarget);
                    }}
                  >
                    Earlier thread · parent not loaded
                  </button>
                )}
                <Message
                  message={row.message}
                  members={channel.members}
                  replies={Math.max(
                    row.thread.replies.length,
                    row.thread.knownCount -
                      Math.max(1, row.thread.parents.length),
                  )}
                  onThread={openThread}
                  paneId={paneId}
                  threadExpanded={thread?.id === row.thread.id}
                />
              </div>
            ),
          )}
        </section>
      </section>
      {thread && (
        <ThreadPane
          channel={channel}
          thread={thread}
          onClose={close}
          paneId={paneId}
        />
      )}
    </div>
  );
}
