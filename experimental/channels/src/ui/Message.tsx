import { Fragment, memo, type ReactNode, useState } from "react";
import {
  overflowCopy,
  type ParticipantCopy,
  participantCopy,
  pendingCopy,
  receiptCopy,
  referenceCopy,
  repliesCopy,
  routeCopy,
  SHOWN_RECIPIENTS,
} from "../copy.ts";
import type {
  AddressId,
  Directory,
  Message as MessageRow,
  Outgoing,
  TerminalResult,
  TerminalRun,
} from "../model.ts";
import { peekTargetOf } from "../peek.ts";
import {
  type Chat,
  chatPeople,
  type RouteLine,
  type RoutePart,
  sameChat,
} from "../route.ts";
import { participant, type ReplyReference } from "../store.ts";
import { terminalResult } from "../wire.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";
import { PeekControl, usePeek } from "./PeekContext.tsx";

const PREVIEW = 480;

function who(directory: Directory, id: MessageRow["author"]): ParticipantCopy {
  return participantCopy(participant(directory, id));
}
function Name({ person }: { person: ParticipantCopy }) {
  return (
    <>
      <span title={person.title ?? undefined}>{person.name}</span>
      {person.suffix !== null && (
        <span className="name-home"> · {person.suffix}</span>
      )}
    </>
  );
}
function PeekName({
  directory,
  id,
}: {
  directory: Directory;
  id: MessageRow["author"];
}) {
  const scope = usePeek()?.scope ?? null;
  const person = participant(directory, id);
  return (
    <PeekControl
      target={peekTargetOf(person, scope)}
      className="peek-name"
      fallback="none"
    >
      <Name person={participantCopy(person)} />
    </PeekControl>
  );
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
  directory,
  paused,
}: {
  message: MessageRow;
  directory: Directory;
  paused: boolean;
}) {
  const several = message.legs.length > 1;
  return (
    <ul className="receipt" aria-label="Delivery">
      {message.legs.map((leg) => {
        const copy = receiptCopy(leg.stage, paused);
        return (
          <li
            key={leg.to}
            data-stage={leg.stage.stage}
            data-tone={copy.tone}
            title={copy.title}
          >
            {several && (
              <span className="receipt-to">
                <Name person={who(directory, leg.to)} />
              </span>
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
  directory: Directory,
  recipients: readonly AddressId[],
): Variant {
  if (who(directory, message.author).avatar === "terminal") return "output";
  const [only] = recipients;
  return recipients.length === 1 &&
    only !== undefined &&
    who(directory, only).avatar === "terminal"
    ? "command"
    : "text";
}

export type SendToHandler = (
  post: MessageRow,
  alias: string,
  button: HTMLButtonElement,
) => void;
type ThreadOpener = (threadId: string, button: HTMLButtonElement) => void;
function Reference({
  reference,
  message,
  directory,
  onThread,
}: {
  reference: ReplyReference;
  message: MessageRow;
  directory: Directory;
  onThread: ThreadOpener;
}) {
  let label = referenceCopy.earlierLabel;
  let content: ReactNode = referenceCopy.earlier;
  if (reference.kind === "message") {
    const author = who(directory, reference.parent.author);
    const [line = ""] = reference.parent.body.text.split("\n", 1);
    label = referenceCopy.label(author.name, line);
    content = (
      <>
        <span className="reply-ref-author">
          <Name person={author} />:
        </span>
        <span className="reply-ref-text">{line}</span>
      </>
    );
  }
  return (
    <button
      type="button"
      className="reply-ref"
      data-thread-id={message.threadId}
      aria-label={label}
      onClick={(event) => onThread(message.threadId, event.currentTarget)}
    >
      <Icon name="reply" className="icon-12" />
      {content}
    </button>
  );
}
function partKey(part: RoutePart): string {
  return part.kind === "receipt" ? `receipt:${part.id}` : part.kind;
}
function Route({
  line,
  message,
  directory,
  paused,
  onThread,
  onSendTo,
  paneId,
  expanded,
}: {
  line: RouteLine;
  message: MessageRow;
  directory: Directory;
  paused: boolean;
  onThread: ThreadOpener | undefined;
  onSendTo: SendToHandler | undefined;
  paneId: string | undefined;
  expanded: boolean;
}) {
  const offer = (alias: string, text: string) =>
    onSendTo === undefined ? (
      text
    ) : (
      <button
        type="button"
        className="route-link"
        data-thread-id={message.threadId}
        aria-label={routeCopy.sendThis(alias)}
        onClick={(event) => onSendTo(message, alias, event.currentTarget)}
      >
        {text}
      </button>
    );
  function part(part: RoutePart): ReactNode {
    switch (part.kind) {
      case "state":
        return <li className="route-state">{routeCopy.state[part.state]}</li>;
      case "names":
        return (
          <li className="route-names">
            {part.ids.map((id, index) => (
              <Fragment key={id}>
                {index > 0 && ", "}
                <PeekName directory={directory} id={id} />
              </Fragment>
            ))}
          </li>
        );
      case "reason":
        return <li>{part.text}</li>;
      case "pulled-in":
        return <li>{routeCopy.pulledIn}</li>;
      case "receipt": {
        const copy = receiptCopy(part.stage, paused);
        return (
          <li
            data-stage={part.stage.stage}
            data-tone={copy.tone}
            title={copy.title}
          >
            {part.named && (
              <span className="receipt-to">
                <Name person={who(directory, part.id)} />
              </span>
            )}
            <Icon name={copy.icon} className="icon-12" />
            {copy.label}
          </li>
        );
      }
      case "mentioned":
        return <li>{routeCopy.mentioned(part.aliases)}</li>;
      case "send":
        return (
          <li>
            {part.form === "anyway"
              ? part.aliases.map((alias) => (
                  <Fragment key={alias}>
                    {offer(alias, routeCopy.send.anyway)}
                  </Fragment>
                ))
              : [
                  `${routeCopy.send.to} `,
                  ...part.aliases.map((alias, index) => (
                    <Fragment key={alias}>
                      {index > 0 && ", "}
                      {offer(alias, alias)}
                    </Fragment>
                  )),
                ]}
          </li>
        );
      case "replies":
        return (
          <li>
            {onThread === undefined ? (
              repliesCopy(part.count)
            ) : (
              <button
                type="button"
                className="route-link"
                data-thread-id={message.threadId}
                aria-controls={paneId}
                aria-expanded={expanded}
                aria-label={routeCopy.openReplies(part.count)}
                onClick={(event) =>
                  onThread(message.threadId, event.currentTarget)
                }
              >
                {repliesCopy(part.count)}
              </button>
            )}
          </li>
        );
      default: {
        const exhaustive: never = part;
        return exhaustive;
      }
    }
  }
  const { tip } = line;
  return (
    <div
      className="route"
      data-tone={line.tone}
      title={
        tip === null
          ? undefined
          : tip.kind === "router"
            ? routeCopy.tip.router(tip.name)
            : routeCopy.tip.unreachable
      }
    >
      {line.icon !== null && (
        <Icon name={line.icon} className="icon-12 route-icon" />
      )}
      <ul className="route-parts" aria-label={routeCopy.label}>
        {line.parts.map((item) => (
          <Fragment key={partKey(item)}>{part(item)}</Fragment>
        ))}
      </ul>
    </div>
  );
}

type MessageProps = {
  message: MessageRow;
  directory: Directory;
  paused?: boolean;
  recipients: readonly AddressId[];
  grouped?: boolean;
  showRecipients?: boolean;
  receipts?: boolean;
  exit?: TerminalRun | undefined;
  latestReply?: MessageRow | null;
  replyCount?: number;
  replyExit?: TerminalRun | undefined;
  chat?: Chat | null;
  onThread?: ThreadOpener;
  onSendTo?: SendToHandler;
  paneId?: string;
  threadExpanded?: boolean;
  arrive?: boolean;
  located?: boolean;
};
function SystemRow({
  message,
  directory,
  paused,
  arrive,
  located,
}: Pick<MessageProps, "message" | "directory" | "arrive" | "located"> & {
  paused: boolean;
}) {
  const failed = message.legs.some((leg) => leg.stage.stage === "failed");
  const { text, truncated } = message.body;
  return (
    <article
      className={`system${arrive ? " arrive" : ""}${located ? " located" : ""}`}
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
        {failed && (
          <Receipts message={message} directory={directory} paused={paused} />
        )}
      </div>
    </article>
  );
}
function ReplyPreview({
  message,
  directory,
  reply,
  count,
  exit,
  onThread,
  paneId,
  expanded,
}: {
  message: MessageRow;
  directory: Directory;
  reply: MessageRow | null;
  count: number;
  exit: TerminalRun | undefined;
  onThread: ThreadOpener;
  paneId: string | undefined;
  expanded: boolean;
}) {
  const replies = `${count} ${count === 1 ? "reply" : "replies"}`;
  let label = `${replies}. Open thread`;
  let content: ReactNode = null;
  if (reply !== null) {
    const author = who(directory, reply.author);
    const name = author.name;
    const type = author.avatar;
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
            <span className="rp-author">
              <Name person={author} />
            </span>
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
function samePerson(
  before: Directory,
  after: Directory,
  id: MessageRow["author"],
): boolean {
  const a = who(before, id);
  const b = who(after, id);
  return (
    a.name === b.name &&
    a.suffix === b.suffix &&
    a.title === b.title &&
    a.avatar === b.avatar
  );
}
export const Message = memo(
  function Message({
    message,
    directory,
    paused = false,
    recipients,
    grouped = false,
    showRecipients = true,
    receipts = true,
    exit,
    latestReply = null,
    replyCount = 0,
    replyExit,
    chat = null,
    onThread,
    onSendTo,
    paneId,
    threadExpanded = false,
    arrive = false,
    located = false,
  }: MessageProps) {
    if (message.kind === "nudge" || message.kind === "hook")
      return (
        <SystemRow
          message={message}
          directory={directory}
          paused={paused}
          arrive={arrive}
          located={located}
        />
      );
    const author = who(directory, message.author);
    const time = clock(message.at);
    const variant = variantFor(message, directory, recipients);
    const named = recipients.map((id) => who(directory, id));
    const result = variant === "output" ? terminalResult(message.body) : null;
    // A channel-chat message went to the room, so its route line says who got it.
    const channelChat = chat?.kind === "channel";
    return (
      <article
        className={`msg ${grouped ? "grouped" : "group-start"}${arrive ? " arrive" : ""}${located ? " located" : ""}`}
        data-message-id={message.id}
        data-kind={message.kind}
      >
        {chat?.reference && onThread && (
          <Reference
            reference={chat.reference}
            message={message}
            directory={directory}
            onThread={onThread}
          />
        )}
        {grouped ? (
          <time className="msg-gutter" dateTime={time?.iso} title={time?.full}>
            {time?.short}
          </time>
        ) : (
          <Avatar alias={author.name} type={author.avatar} />
        )}
        <div className="msg-content">
          {(!grouped || showRecipients) && (
            <header className="msg-head">
              {!grouped && (
                <span className="author">
                  <PeekName directory={directory} id={message.author} />
                </span>
              )}
              {!grouped && <Exit run={exit} result={result} />}
              {!channelChat && recipients.length > 0 && (
                <span
                  className="to"
                  title={
                    named.length > SHOWN_RECIPIENTS
                      ? named.map((person) => person.name).join(", ")
                      : undefined
                  }
                >
                  <Icon name="arrow" />
                  <span className="sr-only"> to </span>
                  <span>
                    {recipients.slice(0, SHOWN_RECIPIENTS).map((id, index) => (
                      <Fragment key={id}>
                        {index > 0 && ", "}
                        <PeekName directory={directory} id={id} />
                      </Fragment>
                    ))}
                  </span>
                  {named.length > SHOWN_RECIPIENTS && (
                    <span className="overflow">
                      {" "}
                      {overflowCopy(named.length - SHOWN_RECIPIENTS)}
                    </span>
                  )}
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
          {channelChat
            ? chat.route && (
                <Route
                  line={chat.route}
                  message={message}
                  directory={directory}
                  paused={paused}
                  onThread={onThread}
                  onSendTo={onSendTo}
                  paneId={paneId}
                  expanded={threadExpanded}
                />
              )
            : receipts &&
              message.legs.length > 0 && (
                <Receipts
                  message={message}
                  directory={directory}
                  paused={paused}
                />
              )}
          {chat?.kind === "agents" && chat.route && (
            <Route
              line={chat.route}
              message={message}
              directory={directory}
              paused={paused}
              onThread={onThread}
              onSendTo={onSendTo}
              paneId={paneId}
              expanded={threadExpanded}
            />
          )}
          {chat == null && onThread && replyCount > 0 && (
            <ReplyPreview
              message={message}
              directory={directory}
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
    sameChat(before.chat ?? null, after.chat ?? null) &&
    before.onThread === after.onThread &&
    before.onSendTo === after.onSendTo &&
    before.paneId === after.paneId &&
    before.threadExpanded === after.threadExpanded &&
    before.arrive === after.arrive &&
    before.located === after.located &&
    before.paused === after.paused &&
    (before.directory === after.directory ||
      (samePerson(before.directory, after.directory, before.message.author) &&
        before.message.legs.every(({ to }) =>
          samePerson(before.directory, after.directory, to),
        ) &&
        (before.latestReply == null ||
          samePerson(
            before.directory,
            after.directory,
            before.latestReply.author,
          )) &&
        chatPeople(before.chat ?? null).every((id) =>
          samePerson(before.directory, after.directory, id),
        ))),
);

export function PendingMessage({
  entry,
  grouped = false,
}: {
  entry: Outgoing;
  grouped?: boolean;
}) {
  const time = clock(entry.at);
  const stage = entry.phase.kind;
  const copy = pendingCopy[stage];
  return (
    <article
      className={`msg pending ${grouped ? "grouped" : "group-start"}`}
      data-request-id={entry.requestId}
    >
      {grouped ? (
        <time className="msg-gutter" dateTime={time?.iso} title={time?.full}>
          {time?.short}
        </time>
      ) : (
        <Avatar alias="You" type="human" />
      )}
      <div className="msg-content">
        {!grouped && (
          <header className="msg-head">
            <span className="author">You</span>
            {time && (
              <time dateTime={time.iso} title={time.full}>
                {time.label}
              </time>
            )}
          </header>
        )}
        <Body text={entry.source} truncated={false} variant="text" />
        <ul className="receipt" aria-label="Delivery">
          <li data-stage={stage}>
            <Icon name={copy.icon} className="icon-12" />
            {copy.label}
          </li>
        </ul>
      </div>
    </article>
  );
}
