import type { ActivityGroupKind } from "./activity.ts";
import type { DisabledReason } from "./complete.ts";
import type {
  Activity,
  Attention,
  ChannelSnapshot,
  DeliveryStage,
  Health,
  MemberKind,
  OutgoingPhase,
  Participant,
  SendFailure,
} from "./model.ts";

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
  | "pause"
  | "plug"
  | "prompt"
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
  ["settled", done],
  ["no_reply", { ...done, label: "Done, no reply needed" }],
]);
export function receiptCopy(stage: DeliveryStage, paused = false): ReceiptCopy {
  if (paused && stage.stage === "queued") return stageCopy.held;
  if (stage.stage !== "completed") return stageCopy[stage.stage];
  return completedCopy.get(stage.kind) ?? { ...done, title: stage.kind };
}

export type AttentionCopy = {
  title: string;
  hint: string;
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
  agent_status: { ...needsYou, hint: "Open its pane in Herdr to answer." },
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
    hint: "Review the report and close it from the lead's pane.",
  },
  context_grant: {
    title: "Context request waiting",
    named: (alias) => `${alias} is asking for context`,
    hint: "Approve or decline it from the pane.",
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
export function attentionCopy(item: Attention): AttentionCopy {
  const { text, truncated } = item.summary;
  const row = attentionTable[item.source];
  if (
    item.source === "agent_status" &&
    !truncated &&
    text.trim() &&
    text.length <= SHORT_SUMMARY
  )
    return { title: text, hint: row.hint, details: null };
  const details = `${text}${truncated ? "…" : ""}`;
  return item.subjectAlias === null
    ? { title: row.title, hint: row.hint, details }
    : {
        title: row.named(item.subjectAlias),
        hint: row.namedHint ?? row.hint,
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
  retired: "Retired",
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

export const composerCopy = {
  hint: "Enter to send · Shift+Enter for a new line",
  sending: "Sending…",
  send: "Send",
  everyWorkspace: "every workspace",
  needsRecipient: "Start with @name to choose who gets this.",
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
const noOneToReplyTo =
  "No one in this thread can be mentioned. Start with @name to choose someone.";
// The "To" label above the box already names the recipients.
export function replyPlaceholder(recipients: readonly string[]): string {
  return recipients.length === 0 ? noOneToReplyTo : "Write a reply";
}
export function replyLabel(recipients: readonly string[]): string {
  return recipients.length === 0
    ? noOneToReplyTo
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
    "Another workspace shows the same name. Give one a unique label in Herdr, and check that Namedrop is active there.",
  unknown:
    "Namedrop can't mention this workspace by its label. Check that the label is unique and isn't “all”, and that Namedrop is active there.",
} satisfies Record<DisabledReason, string>;

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
