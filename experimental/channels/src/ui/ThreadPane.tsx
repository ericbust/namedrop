import { useEffect, useRef } from "react";
import type { Channel } from "../model.ts";
import type { ThreadGroup } from "../store.ts";
import { Message } from "./Message.tsx";

export function ThreadPane({
  channel,
  thread,
  onClose,
  paneId,
}: {
  channel: Channel;
  thread: ThreadGroup;
  onClose: () => void;
  paneId: string;
}) {
  const pane = useRef<HTMLElement>(null);
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
  return (
    <aside className="thread-pane" aria-label="Thread" id={paneId} ref={pane}>
      <header className="thread-heading">
        <h2>Thread</h2>
        <button
          type="button"
          className="text-button"
          onClick={onClose}
          aria-label="Close thread"
        >
          Close
        </button>
      </header>
      <div className="thread-messages">
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
          />
        ))}
        <p className="reply-divider">
          {thread.replies.length}{" "}
          {thread.replies.length === 1 ? "reply" : "replies"}
          {thread.knownCount > thread.parents.length + thread.replies.length
            ? " loaded · earlier messages may be available"
            : ""}
        </p>
        {thread.replies.map((message) => (
          <Message
            key={message.id}
            message={message}
            members={channel.members}
          />
        ))}
      </div>
    </aside>
  );
}
