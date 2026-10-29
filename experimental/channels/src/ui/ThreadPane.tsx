import { useEffect, useRef, useState } from "react";
import type { Channel } from "../model.ts";
import {
  exitFor,
  type RunIndex,
  recipients,
  type ThreadGroup,
} from "../store.ts";
import { Icon } from "./Icons.tsx";
import { Message } from "./Message.tsx";

export function ThreadPane({
  channel,
  thread,
  runs,
  onClose,
  paneId,
}: {
  channel: Channel;
  thread: ThreadGroup;
  runs: RunIndex;
  onClose: () => void;
  paneId: string;
}) {
  const pane = useRef<HTMLElement>(null);
  const [openedAt] = useState(() => Date.now());
  useEffect(() => {
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      const target = document.activeElement;
      if (target?.closest('dialog, [role="dialog"], [role="alertdialog"]'))
        return;
      const inside = target !== null && pane.current?.contains(target);
      const body =
        target === document.body &&
        ![
          ...document.querySelectorAll(
            'dialog[open], [role="dialog"], [role="alertdialog"]',
          ),
        ].some((dialog) => dialog.getClientRects().length > 0);
      if (inside || body) {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onEscape);
    return () => document.removeEventListener("keydown", onEscape);
  }, [onClose]);
  const count = thread.replies.length;
  const unloaded =
    thread.knownCount > thread.parents.length + thread.replies.length;
  return (
    <aside className="thread" aria-label="Thread" id={paneId} ref={pane}>
      <header className="thread-head">
        <h2 className="thread-title">Thread</h2>
        <span className="thread-where">
          in{" "}
          {channel.addressableLabel === null
            ? channel.displayName
            : `#${channel.addressableLabel}`}
        </span>
        <button
          type="button"
          className="icon-button"
          onClick={onClose}
          aria-label="Close thread"
        >
          <Icon name="x" />
        </button>
      </header>
      <div className="thread-body">
        {thread.parents.length === 0 && (
          <p className="earlier-thread">
            Earlier thread · the parent message is not loaded yet.
          </p>
        )}
        {thread.parents.map((message) => (
          <Message
            key={message.id}
            message={message}
            members={channel.members}
            recipients={recipients(message)}
            exit={exitFor(runs, message)}
          />
        ))}
        <p className="divider">
          {count === 0 && !unloaded
            ? "No replies yet"
            : `${count} ${count === 1 ? "reply" : "replies"}`}
          {unloaded ? " · earlier replies not loaded" : ""}
        </p>
        {thread.replies.map((message) => (
          <Message
            key={message.id}
            message={message}
            members={channel.members}
            recipients={recipients(message)}
            exit={exitFor(runs, message)}
            arrive={message.at > openedAt}
          />
        ))}
      </div>
    </aside>
  );
}
