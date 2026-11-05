import type { AddressId, PendingReminder, SpaceKey } from "./model.ts";

export type ReminderConversation = { key: SpaceKey } & (
  | { kind: "thread"; thread: string }
  | { kind: "direct"; peer: AddressId }
);

export function reminderPath(target: ReminderConversation): string {
  const scope =
    target.kind === "direct"
      ? `direct-messages/${encodeURIComponent(target.peer)}`
      : `threads/${encodeURIComponent(target.thread)}`;
  return `/w/${target.key}/${scope}/reminders`;
}

export function reminderRepeats(reminder: PendingReminder): string {
  if (reminder.repeatUntil !== null)
    return `Repeats until ${new Date(reminder.repeatUntil).toLocaleString()} · up to ${reminder.repeatsLeft} repeats left`;
  return `${reminder.repeatsLeft} ${reminder.repeatsLeft === 1 ? "repeat" : "repeats"} left`;
}
