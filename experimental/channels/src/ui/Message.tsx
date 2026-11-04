import {
  Fragment,
  memo,
  type ReactNode,
  useContext,
  useRef,
  useState,
} from "react";
import { flushSync } from "react-dom";
import {
  bodyCopy,
  overflowCopy,
  type ParticipantCopy,
  participantCopy,
  pendingCopy,
  receiptCopy,
  replyInThreadCopy,
  routeCopy,
  SHOWN_RECIPIENTS,
} from "../copy.ts";
import type {
  AddressId,
  Alias,
  Directory,
  Message as MessageRow,
  Outgoing,
  SafeText,
  TerminalResult,
  TerminalRun,
} from "../model.ts";
import { plainNudge } from "../nudge.ts";
import { peekTargetOf } from "../peek.ts";
import {
  type Chat,
  chatPeople,
  type RouteLine,
  type RoutePart,
  sameChat,
} from "../route.ts";
import { participant, replyTargetName } from "../store.ts";
import { terminalResult } from "../wire.ts";
import { Avatar } from "./Avatar.tsx";
import { DeliveryContext } from "./DeliveryContext.ts";
import { Icon } from "./Icons.tsx";
import { Markdown, MarkdownLine } from "./Markdown.tsx";
import {
  MessageActionsContext,
  ThreadConversationContext,
} from "./MessageActions.tsx";
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
  text: shortText,
  truncated: shortTruncated,
  variant,
  messageId = null,
}: {
  text: string;
  truncated: boolean;
  variant: Variant;
  messageId?: string | null;
}) {
  const actions = useContext(MessageActionsContext);
  const [expanded, setExpanded] = useState(false);
  const [full, setFull] = useState<SafeText | "loading" | "failed" | null>(
    null,
  );
  const loaded = typeof full === "object" ? full : null;
  const text = loaded?.text ?? shortText;
  const truncated = loaded?.truncated ?? shortTruncated;
  const fetchable = shortTruncated && messageId !== null && actions !== null;
  const long = text.length > PREVIEW || (fetchable && loaded === null);
  const limit = long && !expanded ? previewLimit(text) : text.length;
  const more = long && !expanded ? "…" : null;
  const toggle = () => {
    if (expanded || !fetchable || loaded !== null) {
      setExpanded(!expanded);
      return;
    }
    if (full === "loading") return;
    setFull("loading");
    void actions.fullBody(messageId).then((body) => {
      setFull(body ?? "failed");
      if (body !== null) setExpanded(true);
    });
  };
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
            <Markdown source={text} limit={limit} tail={more} />
          )}
        </div>
      )}
      {long && (
        <button
          className="text-button show-more"
          type="button"
          aria-expanded={expanded}
          aria-busy={full === "loading" || undefined}
          onClick={toggle}
        >
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
      {full === "failed" && (
        <p className="truncation-note" role="status">
          {bodyCopy.unavailable}
        </p>
      )}
      {truncated && (!fetchable || loaded !== null) && (
        <p className="truncation-note">{bodyCopy.shortened}</p>
      )}
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
      {truncated && <p className="truncation-note">{bodyCopy.shortened}</p>}
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
        const copy = receiptCopy(
          leg.stage,
          participant(directory, leg.to),
          paused,
        );
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

type ThreadOpener = (threadId: string, button: HTMLButtonElement) => void;
// A delivery part that leaves while it has focus hands focus to its message
// row, so the next Tab continues from there. React runs a ref's cleanup before
// it takes the element off the page.
function focusRowOnLeave(button: HTMLButtonElement) {
  return () => {
    if (document.activeElement !== button) return;
    const row = button.closest<HTMLElement>("article");
    row?.setAttribute("tabindex", "-1");
    row?.focus({ preventScroll: true });
  };
}
function partKey(part: RoutePart): string {
  switch (part.kind) {
    case "receipt":
      return `receipt:${part.id}`;
    case "delivery":
      return `delivery:${part.alias}`;
    default:
      return part.kind;
  }
}
function Route({
  line,
  message,
  directory,
  paused,
}: {
  line: RouteLine;
  message: MessageRow;
  directory: Directory;
  paused: boolean;
}) {
  const deliveries = useContext(DeliveryContext);
  const element = useRef<HTMLDivElement>(null);
  const offerId = (alias: Alias) => `${message.id} ${alias}`;
  const offer = (alias: Alias, text: string) =>
    deliveries === null ? (
      text
    ) : (
      <button
        type="button"
        className="route-link"
        aria-label={routeCopy.sendThis(alias)}
        onClick={() => {
          flushSync(() => deliveries.deliver(message, alias));
          for (const button of element.current?.querySelectorAll<HTMLElement>(
            "[data-offer]",
          ) ?? [])
            if (button.dataset.offer === offerId(alias)) button.focus();
        }}
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
        const copy = receiptCopy(
          part.stage,
          participant(directory, part.id),
          paused,
        );
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
      case "delivery": {
        const { alias, state } = part;
        return (
          <li>
            {state.kind === "failed" && (
              <span className="route-failed">
                {routeCopy.delivery.failed(alias)}
              </span>
            )}
            <button
              type="button"
              className="route-link"
              ref={focusRowOnLeave}
              data-offer={offerId(alias)}
              aria-disabled={state.kind !== "failed" || undefined}
              title={
                state.kind === "failed"
                  ? routeCopy.delivery.why(state)
                  : undefined
              }
              onClick={() => deliveries?.deliver(message, alias)}
            >
              {routeCopy.delivery.label[state.kind](alias)}
            </button>
          </li>
        );
      }
      default: {
        const exhaustive: never = part;
        return exhaustive;
      }
    }
  }
  const { tip } = line;
  return (
    <div
      ref={element}
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
      <Icon name={line.icon} className="icon-12 route-icon" />
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
  latestReply,
  replyCount = 0,
  replyExit,
  onThread,
  paneId,
  threadExpanded = false,
}: Pick<
  MessageProps,
  | "message"
  | "directory"
  | "arrive"
  | "located"
  | "latestReply"
  | "replyCount"
  | "replyExit"
  | "onThread"
  | "paneId"
  | "threadExpanded"
> & {
  paused: boolean;
}) {
  const failed = message.legs.some((leg) => leg.stage.stage === "failed");
  const { text: raw, truncated } = message.body;
  const text = message.kind === "nudge" ? plainNudge(raw) : raw;
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
          <span className="body" title={text === raw ? undefined : raw}>
            <MarkdownLine source={text} limit={previewLimit(text)} links />
            {text.length > PREVIEW || truncated ? "…" : null}
          </span>
        </p>
        {failed && (
          <Receipts message={message} directory={directory} paused={paused} />
        )}
        {onThread && replyCount > 0 && (
          <ReplyPreview
            message={message}
            directory={directory}
            reply={latestReply ?? null}
            count={replyCount}
            exit={replyExit}
            onThread={onThread}
            paneId={paneId}
            expanded={threadExpanded}
          />
        )}
      </div>
      <ReplyInThread message={message} directory={directory} />
    </article>
  );
}
function ReplyInThread({
  message,
  directory,
}: {
  message: MessageRow;
  directory: Directory;
}) {
  const actions = useContext(MessageActionsContext);
  const inThread = useContext(ThreadConversationContext);
  const reply = actions?.reply;
  if (!reply || inThread) return null;
  const name = replyTargetName(message, directory);
  return (
    <div className="msg-actions">
      <button
        type="button"
        className="msg-reply"
        aria-label={`${replyInThreadCopy.label} to ${name}`}
        title={replyInThreadCopy.label}
        onClick={(event) => reply(message, event.currentTarget)}
      >
        <Icon name="reply" className="icon-12" />
        <span>{replyInThreadCopy.label}</span>
      </button>
    </div>
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
              {result ? (
                text.slice(0, previewLimit(text, 400))
              ) : (
                <MarkdownLine source={text} limit={previewLimit(text, 400)} />
              )}
            </span>
          )}
        </span>
      </>
    );
  }
  return (
    <div className="rp-room">
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
    </div>
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
          latestReply={latestReply}
          replyCount={replyCount}
          replyExit={replyExit}
          onThread={onThread}
          paneId={paneId}
          threadExpanded={threadExpanded}
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
                messageId={message.id}
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
              messageId={message.id}
            />
          )}
          {channelChat
            ? chat.route && (
                <Route
                  line={chat.route}
                  message={message}
                  directory={directory}
                  paused={paused}
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
          {onThread && replyCount > 0 && (
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
        <ReplyInThread message={message} directory={directory} />
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
