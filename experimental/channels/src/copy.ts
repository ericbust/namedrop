import type { Activity, Attention, DeliveryStage, Health } from "./model.ts";

export type IconName =
  | "alert"
  | "arrow"
  | "arrowDown"
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
export function receiptCopy(stage: DeliveryStage): ReceiptCopy {
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
