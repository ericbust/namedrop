import {
  Fragment,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { composerCopy, emptyChannelCopy } from "../copy.ts";
import { idle } from "../delivery.ts";
import type { Channel, Directory, Outgoing, SpaceKey } from "../model.ts";
import { answered, chatOf } from "../route.ts";
import {
  exitFor,
  GROUP_WINDOW,
  type RunIndex,
  type TimelineRow,
} from "../store.ts";
import { DeliveryContext } from "./DeliveryContext.ts";
import { Icon } from "./Icons.tsx";
import { Message, PendingMessage } from "./Message.tsx";

// A new object per activation, so showing the same message again re-scrolls.
export type Located = { messageId: string };
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
function screensAbove(element: HTMLElement): boolean {
  return (
    element.scrollHeight - element.scrollTop - element.clientHeight >
    element.clientHeight
  );
}
export function Timeline({
  channel,
  rows,
  runs,
  directory,
  pending,
  posted,
  located,
  unsentReplies,
  expandedThread,
  onThread,
  paneId,
  onOlder,
  onLatest,
  onFollow,
}: {
  channel: Channel;
  rows: readonly TimelineRow[];
  runs: RunIndex;
  directory: Directory;
  pending: readonly Outgoing[];
  posted: ReadonlySet<string>;
  located: Located | null;
  unsentReplies: ReadonlySet<string>;
  expandedThread: string | null;
  onThread: (threadId: string, button: HTMLButtonElement) => void;
  paneId: string;
  onOlder: (key: SpaceKey) => void;
  onLatest: (key: SpaceKey) => void;
  onFollow: (key: SpaceKey, atBottom: boolean) => void;
}) {
  const scroller = useRef<HTMLElement>(null);
  const anchors = useRef(new Map<SpaceKey, Anchor>());
  const deliveries = useContext(DeliveryContext);
  const [far, setFar] = useState(false);
  const shown = useRef<{ key: SpaceKey; at: number } | null>(null);
  if (shown.current?.key !== channel.key)
    shown.current = { key: channel.key, at: Date.now() };
  const shownAt = shown.current.at;
  const shownPending = useRef(pending);
  const following = useRef(channel.key);
  following.current = channel.key;
  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      if (anchors.current.get(following.current)?.bottom)
        element.scrollTop = element.scrollHeight;
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const anchor = anchors.current.get(channel.key);
    const earlier = new Set(
      shownPending.current.map((entry) => entry.requestId),
    );
    const sent = pending.some((entry) => !earlier.has(entry.requestId));
    shownPending.current = pending;
    if (!anchor || anchor.bottom || sent)
      element.scrollTop = element.scrollHeight;
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
    setFar(screensAbove(element));
  }, [channel, pending]);
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element || located === null) return;
    const row = [
      ...element.querySelectorAll<HTMLElement>("[data-message-id]"),
    ].find((row) => row.dataset.messageId === located.messageId);
    if (!row) return;
    const port = element.getBoundingClientRect();
    const box = row.getBoundingClientRect();
    element.scrollTop +=
      box.top +
      box.height / 2 -
      (port.top + element.clientTop + element.clientHeight / 2);
    anchors.current.set(channel.key, capture(element));
  }, [located, channel.key]);
  const chats = useMemo(
    () =>
      rows.map((row) =>
        row.kind === "day" || row.kind === "gap"
          ? null
          : chatOf({
              message: row.message,
              thread: row.thread,
              reference: row.reference,
              directory,
              replies: row.replyCount,
              delivery: (alias) =>
                deliveries?.of(row.message.id, alias) ?? idle,
            }),
      ),
    [rows, directory, deliveries],
  );
  const history = channel.history;
  const gap = channel.runs.length > 1;
  const browsing = channel.viewMode === "browsing";
  const jumpToLatest = () => {
    scroller.current?.focus({ preventScroll: true });
    if (browsing) {
      anchors.current.delete(channel.key);
      onLatest(channel.key);
    } else if (scroller.current)
      scroller.current.scrollTop = scroller.current.scrollHeight;
  };
  const loadOlder = () => onOlder(channel.key);
  const paused = channel.health.level === "paused";
  // A pending post groups with the operator's message above it, as its row will.
  const last = rows.findLast(
    (row) => row.kind === "message" || row.kind === "orphan",
  );
  let previousAt =
    last?.kind === "message" &&
    last.message.author === "human" &&
    last.message.kind !== "nudge" &&
    last.message.kind !== "hook"
      ? last.message.at
      : null;
  return (
    <section
      className="timeline"
      ref={scroller}
      aria-label="Message history"
      // biome-ignore lint/a11y/noNoninteractiveTabindex: Keyboard scrolling requires focus. https://www.w3.org/WAI/standards-guidelines/act/rules/0ssw9k/
      tabIndex={0}
      onScroll={(event) => {
        const anchor = capture(event.currentTarget);
        anchors.current.set(channel.key, anchor);
        if (anchor.bottom !== channel.atBottom)
          onFollow(channel.key, anchor.bottom);
        setFar(screensAbove(event.currentTarget));
        if (
          event.currentTarget.scrollTop < 40 &&
          history.kind === "idle" &&
          !channel.exhausted
        )
          onOlder(channel.key);
      }}
    >
      <div className="history-edge">
        {history.kind === "loading" ? (
          <span role="status">
            {history.request.purpose === "bridge"
              ? "Loading missing messages…"
              : "Loading earlier messages…"}
          </span>
        ) : history.kind === "error" ? (
          <>
            <span role="alert" title={history.reason}>
              Couldn't load earlier messages.
            </span>
            <button type="button" className="text-button" onClick={loadOlder}>
              Try again
            </button>
          </>
        ) : channel.messages.size > 0 && (!channel.exhausted || gap) ? (
          <button type="button" className="text-button" onClick={loadOlder}>
            {gap ? "Load missing messages" : "Load earlier messages"}
          </button>
        ) : channel.messages.size > 0 ? (
          <span>Beginning of channel</span>
        ) : null}
      </div>
      {channel.messages.size === 0 && (
        <div className="empty">
          <Icon name="inbox" />
          <h2>{emptyChannelCopy.title}</h2>
          <p>{emptyChannelCopy.body}</p>
        </div>
      )}
      {rows.map((row, index) => {
        switch (row.kind) {
          case "day":
            return (
              <p className="divider" key={`day:${row.before}`}>
                {row.label}
              </p>
            );
          case "gap":
            return (
              <div className="divider gap" key={`gap:${row.before}`}>
                Some messages are missing here
                <button
                  type="button"
                  className="text-button"
                  disabled={history.kind === "loading"}
                  onClick={loadOlder}
                >
                  Load them
                </button>
              </div>
            );
          default: {
            const { message, thread, latestReply, replyCount, recipients } =
              row;
            const replied =
              thread.room === null
                ? latestReply !== null &&
                  recipients.length > 0 &&
                  recipients.every((id) =>
                    thread.replies.some((reply) => reply.author === id),
                  )
                : answered(thread.room, recipients, message.at, directory);
            return (
              <Fragment key={message.id}>
                {row.kind === "orphan" && (
                  <button
                    type="button"
                    className="text-button earlier-thread"
                    data-thread-id={thread.id}
                    onClick={(event) =>
                      onThread(thread.id, event.currentTarget)
                    }
                  >
                    Earlier thread · parent not loaded
                  </button>
                )}
                <Message
                  message={message}
                  directory={directory}
                  paused={paused}
                  recipients={recipients}
                  grouped={row.grouped}
                  showRecipients={row.showRecipients}
                  receipts={!replied}
                  exit={exitFor(runs, message)}
                  latestReply={latestReply}
                  replyCount={replyCount}
                  replyExit={exitFor(runs, latestReply)}
                  chat={chats[index]}
                  onThread={onThread}
                  paneId={paneId}
                  threadExpanded={expandedThread === thread.id}
                  arrive={message.at > shownAt && !posted.has(message.id)}
                  located={located?.messageId === message.id}
                />
                {unsentReplies.has(thread.id) &&
                  expandedThread !== thread.id &&
                  message === thread.parents.at(-1) && (
                    <button
                      type="button"
                      className="text-button reply-failed"
                      data-thread-id={thread.id}
                      onClick={(event) =>
                        onThread(thread.id, event.currentTarget)
                      }
                    >
                      {composerCopy.replyNotSent}
                    </button>
                  )}
              </Fragment>
            );
          }
        }
      })}
      {pending.map((entry) => {
        const grouped =
          previousAt !== null &&
          entry.at - previousAt >= 0 &&
          entry.at - previousAt <= GROUP_WINDOW;
        previousAt = entry.at;
        return (
          <PendingMessage
            key={entry.requestId}
            entry={entry}
            grouped={grouped}
          />
        );
      })}
      {browsing && (
        <div className="divider window-end">
          Newer messages aren't loaded
          <button type="button" className="text-button" onClick={jumpToLatest}>
            Jump to latest
          </button>
        </div>
      )}
      {(browsing || far) && (
        <div className="jump-dock">
          <button
            type="button"
            className="jump-latest"
            aria-label="Jump to latest messages"
            onClick={jumpToLatest}
          >
            <Icon name="arrowDown" />
            Jump to latest
          </button>
        </div>
      )}
    </section>
  );
}
