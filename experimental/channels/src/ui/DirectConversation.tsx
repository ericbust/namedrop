import { type ReactNode, useLayoutEffect, useMemo, useRef } from "react";
import type { CompletionContext } from "../complete.ts";
import {
  directComposer,
  directKey,
  directLabel,
  directMember,
} from "../direct-messages.ts";
import type { Directory } from "../model.ts";
import { peekTarget } from "../peek.ts";
import { Composer } from "./Composer.tsx";
import { DirectRuntime } from "./DirectSidebar.tsx";
import { Message, PendingMessage } from "./Message.tsx";
import {
  type MessageActions,
  MessageContexts,
  ThreadConversationContext,
} from "./MessageActions.tsx";
import { PeekControl } from "./PeekContext.tsx";
import { Reminders } from "./Reminders.tsx";
import type { DirectMessages } from "./useDirectMessages.ts";

export function DirectConversation({
  dm,
  completion,
  toggle,
}: {
  dm: DirectMessages;
  completion: CompletionContext | null;
  toggle: ReactNode;
}) {
  const peer = dm.selectedPeer;
  const scroller = useRef<HTMLElement>(null);
  const atBottom = useRef(true);
  const directory = useMemo<Directory>(
    () => ({
      members: peer
        ? new Map([[peer.addressId, directMember(peer)]])
        : new Map(),
      homes: new Map(),
    }),
    [peer],
  );
  const context = useMemo<CompletionContext | null>(
    () =>
      peer === null
        ? null
        : {
            current: peer.spaceKey,
            channels: completion?.channels ?? [],
            membersOf: (key) =>
              key === peer.spaceKey
                ? [
                    directMember(peer),
                    ...(completion
                      ?.membersOf(key)
                      .filter((member) => member.id !== peer.addressId) ?? []),
                  ]
                : (completion?.membersOf(key) ?? []),
            directRecipient: { key: peer.spaceKey, member: peer.addressId },
          },
    [peer, completion],
  );
  const actions = useMemo<MessageActions | null>(
    () =>
      peer === null
        ? null
        : {
            fullBody: (messageId) =>
              dm.fullBody(peer.spaceKey, messageId, peer.addressId),
            reply: null,
          },
    [peer, dm.fullBody],
  );
  const selected = peer === null ? null : directKey(peer);
  const previous = useRef(selected);
  useLayoutEffect(() => {
    if (previous.current !== selected) {
      atBottom.current = true;
      previous.current = selected;
    }
    if (atBottom.current && scroller.current)
      scroller.current.scrollTop = scroller.current.scrollHeight;
  });
  if (peer === null || context === null)
    return (
      <>
        <header className="channel-header">
          {toggle}
          <h1>Direct message</h1>
        </header>
        <div className="empty" role="status">
          <h2>
            {dm.directoryError
              ? "Couldn't find this agent"
              : "Finding your conversation…"}
          </h2>
          <p>Your private conversations appear in Direct messages.</p>
        </div>
      </>
    );
  const member = directMember(peer);
  return (
    <>
      <header className="channel-header dm-header">
        {toggle}
        <DirectRuntime runtime={peer.runtime} />
        <div className="dm-heading">
          <h1>{peer.alias}</h1>
          <span>
            #{peer.workspaceLabel.replace(/^#/, "")} ·{" "}
            {peer.paneLabel?.text ? `${peer.paneLabel.text} · ` : ""}
            {peer.presence}
          </span>
        </div>
        {peer.threadId !== null && (
          <Reminders
            target={{
              kind: "direct",
              key: peer.spaceKey,
              peer: peer.addressId,
            }}
            directory={directory}
          />
        )}
        {!peer.closed && (
          <PeekControl
            target={peekTarget(peer.spaceKey, member)}
            className="text-button dm-peek"
            fallback="none"
          >
            Peek
          </PeekControl>
        )}
      </header>
      <MessageContexts deliveries={null} actions={actions}>
        <ThreadConversationContext value={true}>
          <section
            className="timeline dm-timeline"
            ref={scroller}
            aria-label={`Direct conversation with ${directLabel(peer)}`}
            // biome-ignore lint/a11y/noNoninteractiveTabindex: This named scroll region supports PageDown, verified in direct-messages.browser.mjs.
            tabIndex={0}
            onScroll={(event) => {
              const node = event.currentTarget;
              atBottom.current =
                node.scrollHeight - node.scrollTop - node.clientHeight < 60;
            }}
          >
            {dm.history?.earlierOmitted && (
              <button
                className="text-button dm-older"
                type="button"
                disabled={dm.older}
                onClick={() => void dm.loadOlder()}
              >
                {dm.older
                  ? "Loading earlier messages…"
                  : "Load earlier messages"}
              </button>
            )}
            {dm.historyError && (
              <p className="dm-status" role="alert">
                Couldn't update this conversation.{" "}
                <button
                  className="text-button"
                  type="button"
                  onClick={dm.retry}
                >
                  Try again
                </button>
              </p>
            )}
            {!dm.history && !dm.historyError && (
              <p className="dm-status" role="status">
                Loading conversation…
              </p>
            )}
            {dm.history?.messages.length === 0 && dm.pending.length === 0 && (
              <div className="empty">
                <h2>Message {peer.alias} privately</h2>
                <p>Only you and this agent see this conversation.</p>
              </div>
            )}
            {dm.history?.messages.map((message, index, messages) => (
              <Message
                key={message.id}
                message={message}
                directory={directory}
                recipients={message.legs.map((leg) => leg.to)}
                grouped={
                  index > 0 &&
                  messages[index - 1]?.author === message.author &&
                  messages[index - 1]?.kind !== "reminder"
                }
                showRecipients={false}
              />
            ))}
            {dm.pending.map((entry) => (
              <PendingMessage key={entry.requestId} entry={entry} />
            ))}
          </section>
        </ThreadConversationContext>
      </MessageContexts>
      <Composer
        key={selected}
        context={context}
        composer={directComposer(peer)}
        drafts={dm.drafts}
        outbox={dm.outbox}
        placeholder={`Message ${peer.alias} privately`}
        label={`Message ${peer.alias} privately`}
        rule={{ kind: "direct", alias: peer.alias }}
        closed={
          peer.closed
            ? "This agent is closed. You can still read this conversation."
            : null
        }
        notice={<p>Private conversation with {directLabel(peer)}</p>}
        onEdit={(text) => dm.edit(peer, text)}
        onSend={(source) => dm.send(peer, source)}
      />
    </>
  );
}
