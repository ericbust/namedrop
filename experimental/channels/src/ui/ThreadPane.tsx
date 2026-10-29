import { useEffect, useRef } from "react";
import type { Channel } from "../model.ts";
import type { ThreadGroup } from "../store.ts";
import { Message } from "./Message.tsx";

export function ThreadPane({
  channel,
  thread,
  onClose,
}: {
  channel: Channel;
  thread: ThreadGroup;
  onClose: () => void;
}) {
  const close = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    close.current?.focus();
    const onEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onEscape);
    return () => document.removeEventListener("keydown", onEscape);
  }, [onClose]);
  return (
    <aside className="thread-pane" aria-label="Thread">
      <header className="thread-heading">
        <h2>Thread</h2>
        <button
          type="button"
          className="text-button"
          ref={close}
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
          <Message key={message.id} message={message} channel={channel} />
        ))}
        <p className="reply-divider">
          {thread.replies.length}{" "}
          {thread.replies.length === 1 ? "reply" : "replies"}
          {thread.knownCount > thread.parents.length + thread.replies.length
            ? " loaded · earlier messages may be available"
            : ""}
        </p>
        {thread.replies.map((message) => (
          <Message key={message.id} message={message} channel={channel} />
        ))}
      </div>
    </aside>
  );
}
