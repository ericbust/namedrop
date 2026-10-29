import { Fragment, type ReactNode, useState } from "react";
import type {
  ChannelSnapshot,
  DeliveryStage,
  MessageKind,
  Message as MessageRow,
} from "../model.ts";

export function authorName(
  channel: ChannelSnapshot,
  id: MessageRow["author"],
): string {
  return id === "human" ? "You" : (channel.members.get(id)?.alias ?? id);
}
function linkedText(text: string): ReactNode {
  const matches = [...text.matchAll(/https?:\/\/[^\s<>"']+/g)];
  const parts: ReactNode[] = [];
  let start = 0;
  for (const match of matches) {
    const at = match.index;
    const href = match[0].replace(/[.,;!?)]+$/, "");
    parts.push(text.slice(start, at));
    parts.push(
      <a key={at} href={href} target="_blank" rel="noreferrer">
        {href}
      </a>,
    );
    start = at + href.length;
  }
  parts.push(text.slice(start));
  return parts;
}
function formattedText(text: string): ReactNode {
  const parts = text.split(/(```[^\n]*\n[\s\S]*?(?:```|$))/g);
  let offset = 0;
  return parts
    .filter((part) => part.length > 0)
    .map((part) => {
      const key = offset;
      offset += part.length;
      return part.startsWith("```") ? (
        <pre key={key}>
          <code>{part.replace(/^```[^\n]*\n/, "").replace(/```$/, "")}</code>
        </pre>
      ) : (
        <Fragment key={key}>{linkedText(part)}</Fragment>
      );
    });
}
function Body({ message }: { message: MessageRow }) {
  const [expanded, setExpanded] = useState(false);
  const long = message.body.text.length > 480;
  const body =
    long && !expanded
      ? `${message.body.text.slice(0, 480)}…`
      : message.body.text;
  return (
    <>
      <div className="message-body">{formattedText(body)}</div>
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
export function Message({
  message,
  channel,
  replies = 0,
  onThread,
}: {
  message: MessageRow;
  channel: ChannelSnapshot;
  replies?: number;
  onThread?: (button: HTMLButtonElement) => void;
}) {
  const Content = renderers[message.kind];
  const date = new Date(message.at);
  const validDate = Number.isFinite(date.getTime());
  return (
    <article
      className={`message message-${message.kind}`}
      data-message-id={message.id}
    >
      <header className="message-header">
        <strong>{authorName(channel, message.author)}</strong>
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
              {authorName(channel, leg.to)}
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
          onClick={(event) => onThread(event.currentTarget)}
        >
          {replies} {replies === 1 ? "reply" : "replies"}
        </button>
      )}
    </article>
  );
}
