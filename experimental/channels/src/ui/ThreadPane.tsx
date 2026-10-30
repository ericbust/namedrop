import {
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import {
  attentionReplyTo,
  type ReplyIntent,
  replyPrefill,
} from "../attention.ts";
import type { CompletionContext } from "../complete.ts";
import {
  answerInPane,
  repliesCopy,
  replyLabel,
  replyPlaceholder,
  responseCopy,
} from "../copy.ts";
import { idle } from "../delivery.ts";
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
  draftKey,
  exitFor,
  type RunIndex,
  recipients,
  sameComposer,
  type ThreadGroup,
  threadAgents,
} from "../store.ts";
import { Composer } from "./Composer.tsx";
import { DeliveryContext } from "./DeliveryContext.ts";
import { Icon } from "./Icons.tsx";
import { Message, PendingMessage } from "./Message.tsx";
import { useEscape } from "./useEscape.ts";

export type ThreadResponse =
  | { kind: "reply"; intent: ReplyIntent }
  | {
      kind: "message";
      name: string;
      draft: "prepared" | "kept" | "none";
      prepare: () => void;
    }
  | { kind: "pane"; name: string | null; pane: string | null };

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
  response = null,
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
  response?: ThreadResponse | null;
  target?: { messageId: string } | null;
  earlierOmitted?: boolean;
}) {
  const replyIntent = response?.kind === "reply" ? response.intent : null;
  const pane = useRef<HTMLElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const deliveries = useContext(DeliveryContext);
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
  const [keptNotice, endKeptNotice] = useReducer(() => false, draftPreserved);
  const activeReply = draftPreserved ? null : replyIntent;
  const to = useMemo(
    () =>
      activeReply === null ? null : attentionReplyTo(activeReply, directory),
    [directory, activeReply],
  );
  const rule = useMemo<Extract<SendRule, { kind: "thread" }>>(
    () => ({ kind: "thread", to: threadAgents(thread, directory) }),
    [thread, directory],
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
  const [tinted, setTinted] = useState(target?.messageId ?? null);
  useEffect(() => {
    if (tinted === null) return;
    const done = setTimeout(() => setTinted(null), 1500);
    return () => clearTimeout(done);
  }, [tinted]);
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
            located={message.id === tinted}
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
              delivery: (alias) => deliveries?.of(message.id, alias) ?? idle,
            })}
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
            located={message.id === tinted}
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
              delivery: (alias) => deliveries?.of(message.id, alias) ?? idle,
            })}
            arrive={message.at > openedAt && !posted.has(message.id)}
          />
        ))}
        {pending.map((entry) => (
          <PendingMessage key={entry.requestId} entry={entry} />
        ))}
      </div>
      {activeReply !== null && to === null && (
        <p className="thread-answer" role="status">
          The responsible agent is no longer available. Use its pane in Herdr to
          respond.
        </p>
      )}
      {response?.kind === "pane" && (
        <p
          className="thread-answer"
          role="status"
          title={response.pane ?? undefined}
        >
          {answerInPane(response.name)}
        </p>
      )}
      {response?.kind === "message" && (
        <p className="thread-answer" role="status">
          Answer tracked work in the channel.
          {response.draft === "prepared" &&
            ` Your message to ${response.name} is ready there.`}
          {response.draft === "none" && (
            <>
              {" "}
              <button
                type="button"
                className="text-button"
                onClick={response.prepare}
              >
                {responseCopy.message(response.name)}
              </button>
            </>
          )}
        </p>
      )}
      {(response === null || response.kind === "reply") &&
        completion &&
        latest &&
        (activeReply === null || to !== null) && (
          <Composer
            context={completion}
            composer={composer}
            drafts={drafts}
            outbox={outbox}
            placeholder={replyPlaceholder(rule.to)}
            label={replyLabel(rule.to)}
            rule={rule}
            notice={
              replyIntent !== null && keptNotice ? (
                <p role="status">
                  Your existing draft was kept. Send or clear it before
                  preparing this reply.
                </p>
              ) : null
            }
            onEdit={(text) => {
              endKeptNotice();
              onEdit(composer, text);
            }}
            onSend={(source, startedAt) => {
              endKeptNotice();
              onSend(
                {
                  composer,
                  source,
                  inReplyTo: activeReply?.messageId ?? latest.id,
                },
                startedAt,
              );
            }}
          />
        )}
    </aside>
  );
}
