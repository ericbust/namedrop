import { Fragment, memo, type ReactNode, useState } from "react";
import { receiptCopy } from "../copy.ts";
import type {
  AddressId,
  ChannelSnapshot,
  MemberKind,
  Message as MessageRow,
  TerminalResult,
  TerminalRun,
} from "../model.ts";
import { terminalResult } from "../wire.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";

type Members = ChannelSnapshot["members"];
const PREVIEW = 480;

export function authorName(members: Members, id: MessageRow["author"]): string {
  return id === "human" ? "You" : (members.get(id)?.alias ?? id);
}
function authorType(
  members: Members,
  id: MessageRow["author"],
): MemberKind["type"] {
  return id === "human" ? "human" : (members.get(id)?.kind.type ?? "unknown");
}
function clock(at: number) {
  const date = new Date(at);
  if (!Number.isFinite(date.getTime())) return null;
  const label = date.toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
  return {
    iso: date.toISOString(),
    label,
    short: label.replace(/\s*[AaPp]\.?\s?[Mm]\.?$/, ""),
    full: date.toLocaleString(),
  };
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
  return parts.flatMap((part, index) => {
    const key = offset;
    offset += part.length;
    const available = Math.min(part.length, limit - key);
    if (available <= 0) return [];
    if (part.startsWith("```"))
      return (
        <pre key={key}>
          <code>
            {part
              .slice(0, available)
              .replace(/^```[^\n]*\n/, "")
              .replace(/```$/, "")}
          </code>
        </pre>
      );
    // The pre is a block, so a newline beside a fence would render as an empty line.
    const start = index > 0 && part.startsWith("\n") ? 1 : 0;
    const end =
      index < parts.length - 1 && part.endsWith("\n")
        ? part.length - 1
        : part.length;
    return (
      <Fragment key={key}>
        {linkedText(part.slice(start, end), available - start)}
      </Fragment>
    );
  });
}
function previewLimit(text: string, length = PREVIEW): number {
  const limit = Math.min(length, text.length);
  const previous = text.charCodeAt(limit - 1);
  const next = text.charCodeAt(limit);
  return previous >= 0xd800 &&
    previous <= 0xdbff &&
    next >= 0xdc00 &&
    next <= 0xdfff
    ? limit - 1
    : limit;
}
type Variant = "text" | "command" | "output";
function Body({
  text,
  truncated,
  variant,
}: {
  text: string;
  truncated: boolean;
  variant: Variant;
}) {
  const [expanded, setExpanded] = useState(false);
  const long = text.length > PREVIEW;
  const limit = long && !expanded ? previewLimit(text) : text.length;
  const more = long && !expanded ? "…" : null;
  return (
    <>
      {variant === "output" ? (
        <pre className="output">
          {text.slice(0, limit)}
          {more}
        </pre>
      ) : (
        <div className="body">
          {variant === "command" ? (
            <span className="command">
              <Icon name="prompt" />
              {text.slice(0, limit)}
              {more}
            </span>
          ) : (
            <>
              {formattedText(text, limit)}
              {more}
            </>
          )}
        </div>
      )}
      {long && (
        <button
          className="text-button show-more"
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
      {truncated && <p className="truncation-note">Shortened by Namedrop.</p>}
    </>
  );
}
function TerminalOutput({
  result: { stdout, stderr },
  truncated,
}: {
  result: TerminalResult;
  truncated: boolean;
}) {
  return (
    <>
      {stdout === "" && stderr === "" && <p className="no-output">No output</p>}
      {stdout !== "" && (
        <Body text={stdout} truncated={false} variant="output" />
      )}
      {stderr !== "" && (
        <>
          <p className="stderr-label">stderr</p>
          <Body text={stderr} truncated={false} variant="output" />
        </>
      )}
      {truncated && <p className="truncation-note">Shortened by Namedrop.</p>}
    </>
  );
}
function Exit({
  run,
  result,
}: {
  run: TerminalRun | undefined;
  result: TerminalResult | null;
}) {
  const code = run?.exitCode ?? result?.exitCode ?? null;
  if (code === null) return null;
  const failed = code !== 0;
  return (
    <span className="exit" data-failed={failed ? "" : undefined}>
      <Icon name={failed ? "alert" : "done"} className="icon-12" />
      Exit {code}
    </span>
  );
}
function Receipts({
  message,
  members,
}: {
  message: MessageRow;
  members: Members;
}) {
  const several = message.legs.length > 1;
  return (
    <ul className="receipt" aria-label="Delivery">
      {message.legs.map((leg) => {
        const copy = receiptCopy(leg.stage);
        return (
          <li
            key={leg.to}
            data-stage={leg.stage.stage}
            data-tone={copy.tone}
            title={copy.title}
          >
            {several && (
              <span className="receipt-to">{authorName(members, leg.to)}</span>
            )}
            <Icon name={copy.icon} className="icon-12" />
            {copy.label}
          </li>
        );
      })}
    </ul>
  );
}
function variantFor(
  message: MessageRow,
  members: Members,
  recipients: readonly AddressId[],
): Variant {
  if (authorType(members, message.author) === "terminal") return "output";
  const [only] = recipients;
  return recipients.length === 1 &&
    only !== undefined &&
    members.get(only)?.kind.type === "terminal"
    ? "command"
    : "text";
}

type MessageProps = {
  message: MessageRow;
  members: Members;
  recipients: readonly AddressId[];
  grouped?: boolean;
  showRecipients?: boolean;
  receipts?: boolean;
  exit?: TerminalRun | undefined;
  latestReply?: MessageRow | null;
  replyCount?: number;
  replyExit?: TerminalRun | undefined;
  onThread?: (threadId: string, button: HTMLButtonElement) => void;
  paneId?: string;
  threadExpanded?: boolean;
  arrive?: boolean;
};
function SystemRow({
  message,
  members,
  arrive,
}: Pick<MessageProps, "message" | "members" | "arrive">) {
  const failed = message.legs.some((leg) => leg.stage.stage === "failed");
  const { text, truncated } = message.body;
  return (
    <article
      className={`system${arrive ? " arrive" : ""}`}
      data-message-id={message.id}
      data-kind={message.kind}
    >
      <Icon name={message.kind === "hook" ? "plug" : "bell"} />
      <div className="msg-content">
        <p>
          <strong>{message.kind === "hook" ? "Hook" : "Nudge"}</strong>
          <span className="body">
            {formattedText(text, previewLimit(text))}
            {text.length > PREVIEW || truncated ? "…" : null}
          </span>
        </p>
        {failed && <Receipts message={message} members={members} />}
      </div>
    </article>
  );
}
function ReplyPreview({
  message,
  members,
  reply,
  count,
  exit,
  onThread,
  paneId,
  expanded,
}: {
  message: MessageRow;
  members: Members;
  reply: MessageRow | null;
  count: number;
  exit: TerminalRun | undefined;
  onThread: NonNullable<MessageProps["onThread"]>;
  paneId: string | undefined;
  expanded: boolean;
}) {
  const replies = `${count} ${count === 1 ? "reply" : "replies"}`;
  let label = `${replies}. Open thread`;
  let content: ReactNode = null;
  if (reply !== null) {
    const name = authorName(members, reply.author);
    const type = authorType(members, reply.author);
    const time = clock(reply.at);
    const result = type === "terminal" ? terminalResult(reply.body) : null;
    const text = result
      ? (result.stdout || result.stderr).split("\n", 2).join("\n")
      : reply.body.text;
    label = `${replies}${count === 1 ? "" : ", latest"} from ${name}${time ? ` at ${time.label}` : ""}. Open thread`;
    content = (
      <>
        <Avatar alias={name} type={type} small />
        <span className="rp-main">
          <span className="rp-head">
            <span className="rp-author">{name}</span>
            <Exit run={exit} result={result} />
            {time && (
              <time dateTime={time.iso} title={time.full}>
                {time.label}
              </time>
            )}
          </span>
          {result && text === "" ? (
            <span className="rp-body no-output">No output</span>
          ) : (
            <span className={`rp-body${type === "terminal" ? " mono" : ""}`}>
              {text.slice(0, previewLimit(text, 400))}
            </span>
          )}
        </span>
      </>
    );
  }
  return (
    <button
      type="button"
      className={`reply-preview${reply === null ? " count-only" : ""}`}
      data-thread-id={message.threadId}
      aria-controls={paneId}
      aria-expanded={expanded}
      aria-label={label}
      onClick={(event) => onThread(message.threadId, event.currentTarget)}
    >
      {content}
      <span className="rp-count">{replies}</span>
    </button>
  );
}
function sameMember(
  before: Members,
  after: Members,
  id: MessageRow["author"],
): boolean {
  if (id === "human") return true;
  const a = before.get(id);
  const b = after.get(id);
  return a === b || (a?.alias === b?.alias && a?.kind.type === b?.kind.type);
}
export const Message = memo(
  function Message({
    message,
    members,
    recipients,
    grouped = false,
    showRecipients = true,
    receipts = true,
    exit,
    latestReply = null,
    replyCount = 0,
    replyExit,
    onThread,
    paneId,
    threadExpanded = false,
    arrive = false,
  }: MessageProps) {
    if (message.kind === "nudge" || message.kind === "hook")
      return <SystemRow message={message} members={members} arrive={arrive} />;
    const name = authorName(members, message.author);
    const time = clock(message.at);
    const variant = variantFor(message, members, recipients);
    const result = variant === "output" ? terminalResult(message.body) : null;
    return (
      <article
        className={`msg ${grouped ? "grouped" : "group-start"}${arrive ? " arrive" : ""}`}
        data-message-id={message.id}
        data-kind={message.kind}
      >
        {grouped ? (
          <time className="msg-gutter" dateTime={time?.iso} title={time?.full}>
            {time?.short}
          </time>
        ) : (
          <Avatar alias={name} type={authorType(members, message.author)} />
        )}
        <div className="msg-content">
          {(!grouped || showRecipients) && (
            <header className="msg-head">
              {!grouped && <span className="author">{name}</span>}
              {!grouped && <Exit run={exit} result={result} />}
              {recipients.length > 0 && (
                <span className="to">
                  <Icon name="arrow" />
                  <span className="sr-only"> to </span>
                  {recipients.map((id) => authorName(members, id)).join(", ")}
                </span>
              )}
              {message.kind === "handoff" && (
                <span className="kind">Handoff</span>
              )}
              {!grouped && time && (
                <time dateTime={time.iso} title={time.full}>
                  {time.label}
                </time>
              )}
            </header>
          )}
          {message.kind === "result_set" ? (
            <div className="report">
              <h2 className="report-title">
                <Icon name="report" />
                Coordination report
              </h2>
              <Body
                text={message.body.text}
                truncated={message.body.truncated}
                variant="text"
              />
            </div>
          ) : result ? (
            <TerminalOutput
              result={result}
              truncated={message.body.truncated}
            />
          ) : (
            <Body
              text={message.body.text}
              truncated={message.body.truncated}
              variant={variant}
            />
          )}
          {receipts && message.legs.length > 0 && (
            <Receipts message={message} members={members} />
          )}
          {onThread && replyCount > 0 && (
            <ReplyPreview
              message={message}
              members={members}
              reply={latestReply}
              count={replyCount}
              exit={replyExit}
              onThread={onThread}
              paneId={paneId}
              expanded={threadExpanded}
            />
          )}
        </div>
      </article>
    );
  },
  (before, after) =>
    before.message === after.message &&
    before.recipients === after.recipients &&
    before.grouped === after.grouped &&
    before.showRecipients === after.showRecipients &&
    before.receipts === after.receipts &&
    before.exit === after.exit &&
    before.latestReply === after.latestReply &&
    before.replyCount === after.replyCount &&
    before.replyExit === after.replyExit &&
    before.onThread === after.onThread &&
    before.paneId === after.paneId &&
    before.threadExpanded === after.threadExpanded &&
    before.arrive === after.arrive &&
    (before.members === after.members ||
      (sameMember(before.members, after.members, before.message.author) &&
        before.recipients.every((id) =>
          sameMember(before.members, after.members, id),
        ) &&
        (before.latestReply == null ||
          sameMember(
            before.members,
            after.members,
            before.latestReply.author,
          )))),
);
