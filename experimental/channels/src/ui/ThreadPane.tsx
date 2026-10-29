import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CompletionContext } from "../complete.ts";
import { repliesCopy, replyLabel, replyPlaceholder } from "../copy.ts";
import type {
  Channel,
  ComposerId,
  Directory,
  Draft,
  DraftKey,
  Outgoing,
  SendRule,
} from "../model.ts";
import { chatOf } from "../route.ts";
import {
  exitFor,
  type RunIndex,
  recipients,
  sameComposer,
  type ThreadGroup,
  threadAgents,
} from "../store.ts";
import { Composer } from "./Composer.tsx";
import { Icon } from "./Icons.tsx";
import { Message, PendingMessage, type SendToHandler } from "./Message.tsx";
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
  onSendTo,
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
  onSendTo: SendToHandler;
  onClose: () => void;
  paneId: string;
}) {
  const pane = useRef<HTMLElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [openedAt] = useState(() => Date.now());
  const rule = useMemo<Extract<SendRule, { kind: "thread" }>>(
    () => ({ kind: "thread", to: threadAgents(thread, directory) }),
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
            chat={chatOf({
              message,
              thread,
              reference: null,
              directory,
              replies: 0,
            })}
            onSendTo={onSendTo}
          />
        ))}
        <p className="divider">
          {count === 0 && !unloaded ? "No replies yet" : repliesCopy(count)}
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
            chat={chatOf({
              message,
              thread,
              reference: null,
              directory,
              replies: 0,
            })}
            onSendTo={onSendTo}
            arrive={message.at > openedAt && !posted.has(message.id)}
          />
        ))}
        {pending.map((entry) => (
          <PendingMessage
            key={entry.requestId}
            entry={entry}
            members={channel.members}
          />
        ))}
      </div>
      {completion && latest && (
        <Composer
          context={completion}
          composer={composer}
          drafts={drafts}
          outbox={outbox}
          placeholder={replyPlaceholder(rule.to)}
          label={replyLabel(rule.to)}
          rule={rule}
          onEdit={(text) => onEdit(composer, text)}
          onSend={(source, startedAt) =>
            onSend({ composer, source, inReplyTo: latest.id }, startedAt)
          }
        />
      )}
    </aside>
  );
}
