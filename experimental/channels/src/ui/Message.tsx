import { Fragment, memo, type ReactNode, useState } from "react";
import type {
  ChannelSnapshot,
  DeliveryStage,
  MessageKind,
  Message as MessageRow,
} from "../model.ts";

export function authorName(
  members: ChannelSnapshot["members"],
  id: MessageRow["author"],
): string {
  return id === "human" ? "You" : (members.get(id)?.alias ?? id);
}
function linkedText(text: string, limit: number): ReactNode {
  const parts: ReactNode[] = [];
  let start = 0;
  for (const match of text.matchAll(/https?:\/\/[^\s<>"']+/g)) {
    const at = match.index;
    if (at >= limit) break;
    const href = match[0].replace(/[.,;!?)]+$/, "");
    parts.push(text.slice(start, Math.min(at, limit)));
    parts.push(
      <a key={at} href={href} target="_blank" rel="noreferrer">
        {href.slice(0, limit - at)}
      </a>,
    );
    start = at + href.length;
  }
  if (start < limit) parts.push(text.slice(start, limit));
  return parts;
}
function formattedText(text: string, limit: number): ReactNode {
  const parts = text.split(/(```[^\n]*\n[\s\S]*?(?:```|$))/g);
  let offset = 0;
  return parts.flatMap((part) => {
    const key = offset;
    offset += part.length;
    const available = Math.min(part.length, limit - key);
    if (available <= 0) return [];
    return part.startsWith("```") ? (
      <pre key={key}>
        <code>
          {part
            .slice(0, available)
            .replace(/^```[^\n]*\n/, "")
            .replace(/```$/, "")}
        </code>
      </pre>
    ) : (
      <Fragment key={key}>{linkedText(part, available)}</Fragment>
    );
  });
}
function previewLimit(text: string): number {
  const limit = Math.min(480, text.length);
  const previous = text.charCodeAt(limit - 1);
  const next = text.charCodeAt(limit);
  return previous >= 0xd800 &&
    previous <= 0xdbff &&
    next >= 0xdc00 &&
    next <= 0xdfff
    ? limit - 1
    : limit;
}
function Body({ message }: { message: MessageRow }) {
  const [expanded, setExpanded] = useState(false);
  const long = message.body.text.length > 480;
  const limit =
    long && !expanded
      ? previewLimit(message.body.text)
      : message.body.text.length;
  return (
    <>
      <div className="message-body">
        {formattedText(message.body.text, limit)}
        {long && !expanded ? "…" : null}
      </div>
      {long && (
        <button
          className="text-button"
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
      {message.body.truncated && (
        <p className="truncation-note">
          Message shortened by the server. Only the available text is shown.
        </p>
      )}
    </>
  );
}
type Renderer = (props: { message: MessageRow }) => ReactNode;
const renderers = {
  mention: Body,
  handoff: ({ message }) => (
    <>
      <span className="message-label">Handoff</span>
      <Body message={message} />
    </>
  ),
  reply: Body,
  result_set: ({ message }) => (
    <div className="report-card">
      <h3>Coordination report</h3>
      <Body message={message} />
    </div>
  ),
  nudge: ({ message }) => (
    <div className="system-body">
      <span className="message-label">Nudge</span>
      <Body message={message} />
    </div>
  ),
  hook: ({ message }) => (
    <div className="system-body">
      <span className="message-label">Hook</span>
      <Body message={message} />
    </div>
  ),
  unknown: ({ message }) => (
    <>
      <span className="message-label">Message</span>
      <Body message={message} />
    </>
  ),
} satisfies Record<MessageKind, Renderer>;
function receipt(stage: DeliveryStage): string {
  switch (stage.stage) {
    case "queued":
      return "Queued";
    case "delivered":
      return "Delivered";
    case "working":
      return "Working (acknowledged)";
    case "replied":
      return "Replied";
    case "completed":
      return `Completed · ${stage.kind}`;
    case "held":
      return "Held";
    case "failed":
      return "Failed";
    case "cancelled":
      return "Cancelled";
    case "unknown":
      return "Status unavailable";
    default: {
      const exhaustive: never = stage;
      return exhaustive;
    }
  }
}
type MessageProps = {
  message: MessageRow;
  members: ChannelSnapshot["members"];
  replies?: number;
  onThread?: (threadId: string, button: HTMLButtonElement) => void;
  paneId?: string;
  threadExpanded?: boolean;
};
export const Message = memo(
  function Message({
    message,
    members,
    replies = 0,
    onThread,
    paneId,
    threadExpanded = false,
  }: MessageProps) {
    const Content = renderers[message.kind];
    const date = new Date(message.at);
    const validDate = Number.isFinite(date.getTime());
    return (
      <article
        className={`message message-${message.kind}`}
        data-message-id={message.id}
      >
        <header className="message-header">
          <strong>{authorName(members, message.author)}</strong>
          {validDate && (
            <time dateTime={date.toISOString()} title={date.toLocaleString()}>
              {date.toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
              })}
            </time>
          )}
        </header>
        <Content message={message} />
        {message.legs.length > 0 && (
          <ul className="receipts" aria-label="Delivery receipts">
            {message.legs.map((leg) => (
              <li key={leg.to} data-stage={leg.stage.stage}>
                {authorName(members, leg.to)}
                <span aria-hidden="true"> · </span>
                {receipt(leg.stage)}
              </li>
            ))}
          </ul>
        )}
        {onThread && replies > 0 && (
          <button
            type="button"
            className="text-button thread-button"
            data-thread-id={message.threadId}
            aria-controls={paneId}
            aria-expanded={threadExpanded}
            onClick={(event) => onThread(message.threadId, event.currentTarget)}
          >
            {replies} {replies === 1 ? "reply" : "replies"}
          </button>
        )}
      </article>
    );
  },
  (before, after) =>
    before.message === after.message &&
    before.replies === after.replies &&
    before.onThread === after.onThread &&
    before.paneId === after.paneId &&
    before.threadExpanded === after.threadExpanded &&
    (before.members === after.members ||
      (authorName(before.members, before.message.author) ===
        authorName(after.members, after.message.author) &&
        before.message.legs.every(
          (leg) =>
            authorName(before.members, leg.to) ===
            authorName(after.members, leg.to),
        ))),
);
