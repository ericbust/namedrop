import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CompletionContext } from "../complete.ts";
import { replyLabel, replyPlaceholder } from "../copy.ts";
import type {
  Channel,
  ComposerId,
  Directory,
  Draft,
  DraftKey,
  Outgoing,
} from "../model.ts";
import {
  exitFor,
  type RunIndex,
  recipients,
  replyRecipients,
  sameComposer,
  type ThreadGroup,
} from "../store.ts";
import { Composer } from "./Composer.tsx";
import { Icon } from "./Icons.tsx";
import { Message, PendingMessage } from "./Message.tsx";

export function ThreadPane({
  channel,
  thread,
  runs,
  directory,
  completion,
  drafts,
  outbox,
  posted,
  onEdit,
  onSend,
  onClose,
  paneId,
}: {
  channel: Channel;
  thread: ThreadGroup;
  runs: RunIndex;
  directory: Directory;
  completion: CompletionContext | null;
  drafts: ReadonlyMap<DraftKey, Draft>;
  outbox: ReadonlyMap<string, Outgoing>;
  posted: ReadonlySet<string>;
  onEdit: (composer: ComposerId, text: string) => void;
  onSend: (
    outgoing: Pick<Outgoing, "composer" | "source" | "inReplyTo">,
    startedAt: number,
  ) => void;
  onClose: () => void;
  paneId: string;
}) {
  const pane = useRef<HTMLElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [openedAt] = useState(() => Date.now());
  const to = useMemo(
    () => replyRecipients(thread, directory),
    [thread, directory],
  );
  const latest = thread.replies.at(-1) ?? thread.parents.at(-1);
  const composer = useMemo<ComposerId>(
    () => ({ key: channel.key, thread: thread.id }),
    [channel.key, thread.id],
  );
  const pending = useMemo(
    () =>
      [...outbox.values()].filter((entry) =>
        sameComposer(entry.composer, composer),
      ),
    [outbox, composer],
  );
  const shownPending = useRef(pending);
  useLayoutEffect(() => {
    const earlier = new Set(
      shownPending.current.map((entry) => entry.requestId),
    );
    shownPending.current = pending;
    if (body.current && pending.some((entry) => !earlier.has(entry.requestId)))
      body.current.scrollTop = body.current.scrollHeight;
  }, [pending]);
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
      <div className="thread-body" ref={body}>
        {thread.parents.length === 0 && (
          <p className="earlier-thread">
            Earlier thread · the parent message is not loaded yet.
          </p>
        )}
        {thread.parents.map((message) => (
          <Message
            key={message.id}
            message={message}
            directory={directory}
            paused={channel.health.level === "paused"}
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
            directory={directory}
            paused={channel.health.level === "paused"}
            recipients={recipients(message)}
            exit={exitFor(runs, message)}
            arrive={message.at > openedAt && !posted.has(message.id)}
          />
        ))}
        {pending.map((entry) => (
          <PendingMessage key={entry.requestId} entry={entry} />
        ))}
      </div>
      {completion && latest && (
        <Composer
          context={completion}
          composer={composer}
          drafts={drafts}
          outbox={outbox}
          placeholder={replyPlaceholder(to?.names ?? [])}
          label={replyLabel(to?.names ?? [])}
          to={to}
          onEdit={(text) => onEdit(composer, text)}
          onSend={(source, startedAt) =>
            onSend({ composer, source, inReplyTo: latest.id }, startedAt)
          }
        />
      )}
    </aside>
  );
}
