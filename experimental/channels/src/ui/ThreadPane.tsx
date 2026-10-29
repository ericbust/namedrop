import { useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  attentionReplyTo,
  type ReplyIntent,
  replyPrefill,
} from "../attention.ts";
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
  draftKey,
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
import { useEscape } from "./useEscape.ts";

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
  replyIntent = null,
  target = null,
  earlierOmitted = false,
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
  replyIntent?: ReplyIntent | null;
  target?: { messageId: string } | null;
  earlierOmitted?: boolean;
}) {
  const pane = useRef<HTMLElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [openedAt] = useState(() => Date.now());
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
  const [draftPreserved] = useState(
    () =>
      replyIntent !== null &&
      ((drafts.get(draftKey(composer))?.text.length ?? 0) > 0 ||
        pending.length > 0),
  );
  const activeReply = draftPreserved ? null : replyIntent;
  const to = useMemo(
    () =>
      activeReply === null
        ? replyRecipients(thread, directory)
        : attentionReplyTo(activeReply, directory),
    [thread, directory, activeReply],
  );
  const prefilled = useRef<ReplyIntent | null>(null);
  useLayoutEffect(() => {
    if (
      activeReply === null ||
      to === null ||
      prefilled.current === activeReply
    )
      return;
    prefilled.current = activeReply;
    const prefill = replyPrefill(
      drafts.get(draftKey(composer))?.text ?? "",
      to,
    );
    if (!prefill.preserved) onEdit(composer, prefill.text);
  }, [activeReply, to, drafts, composer, onEdit]);
  useLayoutEffect(() => {
    if (target === null) return;
    const row = body.current?.querySelector<HTMLElement>(
      `[data-message-id="${CSS.escape(target.messageId)}"]`,
    );
    row?.scrollIntoView({ block: "center" });
    row?.setAttribute("tabindex", "-1");
    row?.focus({ preventScroll: true });
  }, [target]);
  const shownPending = useRef(pending);
  useLayoutEffect(() => {
    const earlier = new Set(
      shownPending.current.map((entry) => entry.requestId),
    );
    shownPending.current = pending;
    if (body.current && pending.some((entry) => !earlier.has(entry.requestId)))
      body.current.scrollTop = body.current.scrollHeight;
  }, [pending]);
  useEscape(pane, onClose);
  const count = thread.replies.length;
  const unloaded =
    thread.knownCount > thread.parents.length + thread.replies.length;
  return (
    <aside className="slot thread" aria-label="Thread" id={paneId} ref={pane}>
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
        {earlierOmitted && (
          <p className="earlier-thread">
            Earlier conversation context omitted.
          </p>
        )}
        {thread.parents.length === 0 && (
          <p className="earlier-thread">
            Earlier thread · the parent message is not loaded yet.
          </p>
        )}
        {thread.parents.map((message) => (
          <Message
            key={message.id}
            message={message}
            located={message.id === target?.messageId}
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
            located={message.id === target?.messageId}
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
      {replyIntent !== null && draftPreserved && (
        <p className="earlier-thread" role="status">
          Your existing draft was kept. Send or clear it before preparing this
          reply.
        </p>
      )}
      {activeReply !== null && to === null && (
        <p className="earlier-thread" role="status">
          The responsible agent is no longer available. Use its pane in Herdr to
          respond.
        </p>
      )}
      {completion && latest && (activeReply === null || to !== null) && (
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
            onSend(
              {
                composer,
                source,
                inReplyTo: activeReply?.messageId ?? latest.id,
              },
              startedAt,
            )
          }
        />
      )}
    </aside>
  );
}
