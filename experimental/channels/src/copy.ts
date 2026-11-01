import type { ActivityGroupKind } from "./activity.ts";
import type { Absence } from "./attention.ts";
import type { DisabledReason } from "./complete.ts";
import { threadControls } from "./control.ts";
import type { Delivery } from "./delivery.ts";
import type {
  Activity,
  Attention,
  AttentionResponse,
  ChannelSnapshot,
  DeliveryStage,
  Health,
  Member,
  MemberKind,
  OutgoingPhase,
  Participant,
  ScreenFailure,
  SendFailure,
  ThreadAction,
} from "./model.ts";
import type { PeekNotice } from "./peek.ts";
import type { RouteState, RouteTip, SendForm } from "./route.ts";
import type { PeekWho } from "./store.ts";

export type IconName =
  | "alert"
  | "arrow"
  | "arrowDown"
  | "arrowUp"
  | "bell"
  | "cancelled"
  | "check"
  | "clock"
  | "done"
  | "hash"
  | "inbox"
  | "menu"
  | "pause"
  | "plug"
  | "prompt"
  | "pulse"
  | "refresh"
  | "reply"
  | "report"
  | "unknown"
  | "working"
  | "x";

export type ReceiptCopy = {
  label: string;
  icon: IconName;
  tone: "muted" | "working" | "held" | "failed";
  title?: string;
};
const done: ReceiptCopy = { label: "Done", icon: "done", tone: "muted" };
const stageCopy = {
  queued: { label: "Queued", icon: "clock", tone: "muted" },
  delivered: { label: "Delivered", icon: "check", tone: "muted" },
  working: { label: "Working on it", icon: "working", tone: "working" },
  replied: { label: "Replied", icon: "reply", tone: "muted" },
  held: { label: "Held while paused", icon: "pause", tone: "held" },
  failed: { label: "Not delivered", icon: "alert", tone: "failed" },
  cancelled: { label: "Cancelled", icon: "cancelled", tone: "muted" },
  unknown: { label: "Status unknown", icon: "unknown", tone: "muted" },
} satisfies Record<Exclude<DeliveryStage["stage"], "completed">, ReceiptCopy>;
const completedCopy: ReadonlyMap<string, ReceiptCopy> = new Map([
  ["no_reply", { ...done, label: "Done, no reply needed" }],
]);
const ended: ReceiptCopy = {
  label: "Ended without an answer",
  icon: "unknown",
  tone: "muted",
};
// A settled turn closed no request. An agent's turn should have answered, as
// Needs-you says; a terminal's command just finished.
function settledCopy(recipient: Participant): ReceiptCopy {
  switch (recipient.kind) {
    case "operator":
      return done;
    case "envoy":
    case "unknown":
      return ended;
    case "member":
      switch (recipient.member.kind.type) {
        case "agent":
          return ended;
        case "terminal":
        case "human":
        case "unknown":
          return done;
        default: {
          const exhaustive: never = recipient.member.kind;
          return exhaustive;
        }
      }
    default: {
      const exhaustive: never = recipient;
      return exhaustive;
    }
  }
}
export function receiptCopy(
  stage: DeliveryStage,
  recipient: Participant,
  paused = false,
): ReceiptCopy {
  if (paused && stage.stage === "queued") return stageCopy.held;
  if (stage.stage !== "completed") return stageCopy[stage.stage];
  if (stage.kind === "settled") return settledCopy(recipient);
  return completedCopy.get(stage.kind) ?? { ...done, title: stage.kind };
}

export type AttentionCopy = {
  title: string;
  hint: string;
  hintTitle: string | null;
  details: string | null;
};
const SHORT_SUMMARY = 90;
type AttentionRow = {
  title: string;
  named: (alias: string) => string;
  hint: string;
  namedHint?: string;
};
const needsYou = {
  title: "Something needs you",
  named: (alias: string) => `${alias} needs you`,
};
const unanswered: AttentionRow = {
  title: "Unanswered request",
  named: (alias) => `${alias} didn't answer a request`,
  hint: "An agent ended its turn without replying.",
  namedHint: "It ended its turn without replying.",
};
const attentionTable: Record<Attention["source"], AttentionRow> = {
  agent_status: { ...needsYou, hint: "Answer it in its pane in Herdr." },
  agent_disposition: unanswered,
  delivery_attempt: unanswered,
  automatic_reply_limit: {
    title: "Reply limit reached",
    named: (alias) => `${alias} hit the reply limit`,
    hint: "Continue or stop this thread from its pane.",
  },
  coordination: {
    title: "Coordination needs a decision",
    named: (alias) => `${alias}'s coordination needs a decision`,
    hint: "Read the report, then answer the lead.",
  },
  context_grant: {
    title: "Shared context failed",
    named: (alias) => `${alias} didn't get its shared context`,
    hint: "Namedrop couldn't capture it. Ask the sender to share it again.",
  },
  pane_binding: {
    title: "An agent's pane changed",
    named: (alias) => `${alias}'s pane changed`,
    hint: "Name the pane again to reconnect it.",
  },
  integration_registration: {
    title: "Agent setup incomplete",
    named: (alias) => `${alias} isn't set up yet`,
    hint: "Start a new session in that pane.",
  },
  unknown: { ...needsYou, hint: "Open Details for the full message." },
};
export const responseCopy = {
  reply: (name: string) => `Reply to ${name}`,
  message: (name: string) => `Message ${name}`,
  pane: (name: string) => `See ${name}'s screen`,
} satisfies Record<
  Exclude<AttentionResponse["kind"], "local">,
  (name: string) => string
>;
export function absenceCopy(absence: Absence): string {
  switch (absence.kind) {
    case "closed":
      return `${absence.alias}'s pane is closed, so it can't take a message.`;
    case "unreachable":
      return `${absence.alias}'s workspace has no label Namedrop can mention, so it can't take a message from here.`;
    case "gone":
      return `${absence.alias ?? "That agent"} isn't in this channel anymore.`;
    default: {
      const exhaustive: never = absence;
      return exhaustive;
    }
  }
}
export function answerInPane(name: string | null): string {
  return name === null
    ? attentionTable.agent_status.hint
    : `Answer it in ${name}'s pane in Herdr.`;
}
export function paneLabelText(
  member: Member | null | undefined,
): string | null {
  const label = member?.paneLabel;
  return label ? `${label.text}${label.truncated ? "…" : ""}` : null;
}
const localHint =
  "Open this workspace in Herdr and choose Namedrop Doctor to review this item.";
const localSetupHint =
  "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration.";
const pausedHint = "The agents paused after 10 replies in a row.";
function responseHint(
  item: Attention,
  responder: Member | null,
): Pick<AttentionCopy, "hint" | "hintTitle"> | null {
  if (item.response?.kind === "local")
    return {
      hint:
        item.source === "integration_registration" ? localSetupHint : localHint,
      hintTitle: null,
    };
  if (item.source === "automatic_reply_limit" && threadControls(item) !== null)
    return { hint: pausedHint, hintTitle: null };
  const who = responder?.alias ?? item.subjectAlias;
  const pane = item.response?.kind === "pane";
  // A context grant that failed is shared again by its sender, not fixed in the pane.
  if (
    who === null ||
    item.source === "context_grant" ||
    !(pane || item.source === "agent_status")
  )
    return null;
  return {
    hint: answerInPane(who),
    hintTitle: pane ? paneLabelText(responder) : null,
  };
}
export const threadControlCopy = {
  continue: {
    label: "Allow 10 more replies",
    pending: "Allowing…",
    title: "Lets the agents send 10 more replies in this thread.",
    failed: "Couldn't allow more replies. Try again.",
  },
  stop: {
    label: "Stop thread",
    pending: "Stopping…",
    title: "Ends agent replies in this thread. You can't undo this.",
    failed: "Couldn't stop the thread. Try again.",
  },
  confirm: "Stop this thread for good?",
  confirmStop: "Stop",
  cancel: "Cancel",
  gone: "This thread is no longer in this workspace.",
  status: (code: number) => `HTTP ${code}`,
} satisfies Record<
  ThreadAction,
  { label: string; pending: string; title: string; failed: string }
> &
  Record<string, unknown>;
// What an item is about, in the Needs-you table's words, for a row that already names the agent.
export function attentionKind(item: Attention): string {
  return attentionTable[item.source].title;
}
// With `absent`, the card's press can't reach the agent, so the hint says why.
export function attentionCopy(
  item: Attention,
  responder: Member | null = null,
  absent: Absence | null = null,
): AttentionCopy {
  const { text, truncated } = item.summary;
  const row = attentionTable[item.source];
  const response =
    absent === null
      ? responseHint(item, responder)
      : { hint: absenceCopy(absent), hintTitle: null };
  const hintTitle = response?.hintTitle ?? null;
  if (
    item.source === "agent_status" &&
    !truncated &&
    text.trim() &&
    text.length <= SHORT_SUMMARY
  )
    return {
      title: text,
      hint: response?.hint ?? row.hint,
      hintTitle,
      details: null,
    };
  const details = `${text}${truncated ? "…" : ""}`;
  return item.subjectAlias === null
    ? { title: row.title, hint: response?.hint ?? row.hint, hintTitle, details }
    : {
        title: row.named(item.subjectAlias),
        hint: response?.hint ?? row.namedHint ?? row.hint,
        hintTitle,
        details,
      };
}

export type HealthCopy = { text: string; tone: "muted" | "held" };
const quiet: HealthCopy = { text: "All quiet", tone: "muted" };
export function healthCopy(health: Health): HealthCopy | null {
  switch (health.level) {
    case "calm":
      return quiet;
    case "busy": {
      const parts = [
        health.working > 0 ? `${health.working} working` : null,
        health.inFlight > 0 ? `${health.inFlight} pending` : null,
      ].filter((part) => part !== null);
      return parts.length > 0
        ? { text: parts.join(" · "), tone: "muted" }
        : quiet;
    }
    case "paused":
      return { text: "Paused · deliveries held", tone: "held" };
    case "inactive":
      return { text: "Not active", tone: "muted" };
    case "needs_you":
    case "unknown":
      return null;
    default: {
      const exhaustive: never = health;
      return exhaustive;
    }
  }
}

export const presenceWord = {
  working: "Working",
  waiting: "Waiting",
  blocked: "Needs you",
  idle: "Idle",
  offline: "Offline",
  retired: "Pane closed",
  unknown: "Unknown",
} satisfies Record<Activity, string>;

const runtimeNames: ReadonlyMap<string, string> = new Map([
  ["claude-code", "Claude Code"],
  ["codex", "Codex"],
  ["opencode", "OpenCode"],
  ["pi", "Pi"],
]);
export function runtimeName(runtime: string | null): string {
  if (runtime === null || !runtime.trim()) return "Agent";
  return (
    runtimeNames.get(runtime) ??
    `${runtime.charAt(0).toUpperCase()}${runtime.slice(1)}`
  );
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
export function relativeAge(ms: number, now: number): string {
  const age = now - ms;
  if (age < MINUTE) return "just now";
  if (age < HOUR) return `${Math.floor(age / MINUTE)} min`;
  if (age < DAY) return `${Math.floor(age / HOUR)} h`;
  return `${Math.floor(age / DAY)} d`;
}

export function avatarHue(alias: string): number {
  let sum = 0;
  for (let index = 0; index < alias.length; index++)
    sum += alias.charCodeAt(index);
  return sum % 6;
}

const sameYearDate = new Intl.DateTimeFormat([], {
  weekday: "long",
  month: "long",
  day: "numeric",
});
const otherYearDate = new Intl.DateTimeFormat([], {
  weekday: "long",
  month: "long",
  day: "numeric",
  year: "numeric",
});
export function dayLabel(dayStart: number, now: number): string {
  const today = dayBounds(now).start;
  if (dayStart === today) return "Today";
  if (dayBounds(dayStart).end === today) return "Yesterday";
  return new Date(dayStart).getFullYear() === new Date(now).getFullYear()
    ? sameYearDate.format(dayStart)
    : otherYearDate.format(dayStart);
}
export function dayBounds(ms: number): { start: number; end: number } {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  const start = date.getTime();
  date.setDate(date.getDate() + 1);
  return { start, end: date.getTime() };
}

export const pendingCopy = {
  sending: { label: "Sending…", icon: "clock" },
  sent: { label: "Sent", icon: "check" },
} satisfies Record<OutgoingPhase["kind"], { label: string; icon: IconName }>;

const couldNotSend = "Namedrop couldn't send this. Try again.";
const sessionExpired = () =>
  "Your session expired. Reload the page to keep posting.";
function serverSays(message: string): string {
  const text = message.trim();
  if (!text) return couldNotSend;
  const sentence = `${text.charAt(0).toUpperCase()}${text.slice(1)}`;
  return /[.!?]$/.test(sentence) ? sentence : `${sentence}.`;
}
const sendErrors: ReadonlyMap<number, (message: string) => string> = new Map([
  [400, serverSays],
  [404, serverSays],
  [409, serverSays],
  [401, sessionExpired],
  [403, sessionExpired],
  [413, () => "This message is too long."],
]);
export function sendErrorCopy({ status, message }: SendFailure): string {
  return (sendErrors.get(status) ?? (() => couldNotSend))(message);
}

// At most two names, then "+N".
function shortList(names: readonly string[]): string {
  const shown = names.slice(0, SHOWN_RECIPIENTS).join(", ");
  return names.length > SHOWN_RECIPIENTS
    ? `${shown} ${overflowCopy(names.length - SHOWN_RECIPIENTS)}`
    : shown;
}
export const composerCopy = {
  hint: "Enter to send · Shift+Enter for a new line",
  toRoom: "Enter posts to the channel · Mention @name to bring someone in",
  checks: (names: readonly string[]) =>
    `Enter posts to the channel · Namedrop checks if it's for ${shortList(names)}`,
  sendsOnlyTo: (names: readonly string[]) =>
    `Enter sends only to ${shortList(names)}`,
  lowercase: (alias: string) => `Names are lowercase: ${alias}`,
  paneClosed: (alias: string) => `${alias}'s pane is closed`,
  notMember: (name: string) => `${name} isn't in this channel`,
  spaceAfter: (alias: string) => `Put a space after ${alias}`,
  nameAfter: "Type a name after @",
  allWithLabels: "#all already means every workspace · Drop the other labels",
  sending: "Sending…",
  send: "Send",
  everyWorkspace: "every workspace",
  needsRecipient: "Start with @name to choose who gets this.",
  needsMessage: (names: readonly string[]) =>
    `Add a message after ${shortList(names)}.`,
  postNotSent: "A post wasn't sent",
  replyNotSent: "Reply not sent · Open thread",
};
export function channelToken(
  channel: Pick<ChannelSnapshot, "addressableLabel">,
): string | null {
  return channel.addressableLabel === null
    ? null
    : `#${channel.addressableLabel}`;
}
export function channelMention(
  channel: Pick<ChannelSnapshot, "addressableLabel" | "displayName">,
): string {
  return channelToken(channel) ?? channel.displayName;
}
export function composerPlaceholder(
  channel: Pick<ChannelSnapshot, "addressableLabel" | "displayName">,
): string {
  return `Message ${channelMention(channel)}`;
}
// The "To" label above the box already names the recipients.
export function replyPlaceholder(recipients: readonly string[]): string {
  return recipients.length === 0
    ? composerCopy.needsRecipient
    : "Write a reply";
}
export function replyLabel(recipients: readonly string[]): string {
  return recipients.length === 0
    ? composerCopy.needsRecipient
    : `Reply to ${recipients.join(", ")}`;
}
export function recipientsLabel(recipients: readonly string[]): string {
  return `To ${recipients.join(", ")}`;
}
export const disabledChannelCopy = {
  "unusable-label":
    "This label has no Latin letters or digits, so Namedrop can't mention it. Rename it in Herdr.",
  "no-label": "This workspace has no label in Herdr. Name it to mention it.",
  "shared-name":
    "Another workspace shows the same name. Give one a unique label in Herdr to mention it.",
  unknown:
    "Namedrop can't mention this workspace by its label. Check that the label is unique and isn't “all”, and that Namedrop is active there.",
} satisfies Record<DisabledReason, string>;

export function repliesCopy(count: number): string {
  return `${count} ${count === 1 ? "reply" : "replies"}`;
}
const sendingTo = (alias: string) => `Sending to ${alias}…`;
const sentTo = (alias: string) => `Sent to ${alias}`;
const couldNotSendTo = (alias: string) => `Couldn't send to ${alias}`;
export const routeCopy = {
  label: "Who got this",
  state: {
    checking: "Checking who this is for…",
    failed: "Couldn't check who this is for",
  } satisfies Record<RouteState, string>,
  pulledIn: "pulled in",
  mentioned: (aliases: readonly string[]) =>
    aliases.length === 1
      ? `Mentioned ${aliases[0]}, not sent`
      : `Mentioned ${aliases.length} agents, not sent`,
  send: { anyway: "Send anyway", to: "Send to" } satisfies Record<
    SendForm,
    string
  >,
  sendThis: (alias: string) => `Send this to ${alias}`,
  delivery: {
    label: {
      sending: sendingTo,
      sent: sentTo,
      failed: () => "Try again",
    } satisfies Record<
      Exclude<Delivery["kind"], "idle">,
      (alias: string) => string
    >,
    failed: couldNotSendTo,
    announce: {
      idle: () => "",
      sending: sendingTo,
      sent: sentTo,
      failed: couldNotSendTo,
    } satisfies Record<Delivery["kind"], (alias: string) => string>,
    why: (failure: Extract<Delivery, { kind: "failed" }>) => {
      switch (failure.cause) {
        case "refused":
          if (failure.status === 0) return "Couldn't reach Namedrop.";
          return failure.message === ""
            ? `HTTP ${failure.status}`
            : failure.message;
        case "left-out":
          return "Namedrop didn't send it. The name may belong to a different agent now.";
        case "unconfirmed":
          return "Namedrop didn't confirm it sent this.";
        default: {
          const exhaustive: never = failure;
          return exhaustive;
        }
      }
    },
  },
  tip: {
    router: (name: string) => `Picked by ${name}`,
    unreachable:
      "Namedrop couldn't reach its router, so it didn't pick who gets this post. The post is saved.",
  } satisfies Record<RouteTip["kind"], unknown>,
  openReplies: (count: number) => `${repliesCopy(count)}. Open thread`,
};
const REFERENCE_LIMIT = 80;
export const referenceCopy = {
  earlier: "Reply to an earlier message",
  label: (author: string, line: string) => {
    const points = Array.from(line);
    const clipped =
      points.length > REFERENCE_LIMIT
        ? `${points
            .slice(0, REFERENCE_LIMIT - 1)
            .join("")
            .trimEnd()}…`
        : line;
    const stop = /[.!?…]$/.test(clipped) ? "" : ".";
    return `Reply to ${author}: ${clipped}${stop} Open thread`;
  },
  earlierLabel: "Reply to an earlier message. Open thread",
};

export const emptyChannelCopy = {
  title: "No messages yet",
  body: "Post here to talk with the agents in this workspace. Mention @name to bring one in.",
};
// A workspace where Namedrop was never turned on has no agents to post to.
// Naming one there turns it on, so the empty channel says how.
export const notActivatedCopy = {
  title: "No agents here yet",
  body: "Name an agent in this workspace to start using it here. In the agent's pane, run",
  command: "namedrop name @alias",
};

export const SHOWN_RECIPIENTS = 2;
export function overflowCopy(hidden: number): string {
  return `+${hidden}`;
}

export type ParticipantCopy = {
  name: string;
  suffix: string | null;
  title: string | null;
  avatar: MemberKind["type"] | "unresolved";
};
export function participantCopy(who: Participant): ParticipantCopy {
  switch (who.kind) {
    case "operator":
      return { name: "You", suffix: null, title: null, avatar: "human" };
    case "member":
      return {
        name: who.member.alias,
        suffix: who.home?.name ?? null,
        title: null,
        avatar: who.member.kind.type,
      };
    case "envoy":
      return {
        name:
          who.home === null
            ? "an agent in another workspace"
            : `an agent in ${who.home.name}`,
        suffix: null,
        title: who.id,
        avatar: "unresolved",
      };
    case "unknown":
      return {
        name: "an unknown agent",
        suffix: null,
        title: who.id,
        avatar: "unresolved",
      };
    default: {
      const exhaustive: never = who;
      return exhaustive;
    }
  }
}

export function timeOfDay(ms: number): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
}
export const activityCopy = {
  title: "All activity",
  groups: { needs_you: "Needs you", waiting: "Waiting", working: "Working" },
  counted: { needs_you: "needs you", waiting: "waiting", working: "working" },
  since: (ms: number) => `since ${timeOfDay(ms)}`,
  held: stageCopy.held.label,
  idleTitle: "Every agent is idle",
  idleBody:
    "Agents show up here while they work, wait on each other, or need you.",
} satisfies {
  groups: Record<ActivityGroupKind, string>;
  counted: Record<ActivityGroupKind, string>;
  [copy: string]: unknown;
};
export const peekCopy = {
  label: (alias: string) => `Screen of ${alias}`,
  where: (channel: string) => `in ${channel}`,
  readAt: (ms: number) => `Read at ${timeOfDay(ms)}`,
  reading: "Reading…",
  readingScreen: (alias: string) => `Reading ${alias}'s screen…`,
  busy: "Busy panes can take a few seconds.",
  empty: "The screen is empty.",
  omitted: "Earlier lines not shown",
  text: "Screen text",
  refresh: "Refresh",
  close: "Close screen",
  tryAgain: "Try again",
};
export const peekFailureCopy = {
  closed: (who) => ({
    title: "Pane closed",
    body: `${who.alias} no longer has a pane Namedrop can read.`,
    retry: false,
  }),
  missing: (who) => ({
    title: `Not in ${who.channel.label} any more`,
    body: `Namedrop can't find ${who.alias} in this workspace.`,
    retry: false,
  }),
  busy: () => ({
    title: "Couldn't read the screen",
    body: "Herdr was busy or didn't answer in time.",
    retry: true,
  }),
  session: () => ({
    title: "Session ended",
    body: "Open Namedrop's dashboard link again to keep reading.",
    retry: false,
  }),
  unreachable: () => ({
    title: "Couldn't reach Namedrop",
    body: "Check that Namedrop is still running.",
    retry: true,
  }),
} satisfies Record<ScreenFailure, (who: PeekWho) => PeekNotice>;
