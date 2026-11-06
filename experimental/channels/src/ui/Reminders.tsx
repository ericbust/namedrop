import { useEffect, useRef, useState } from "react";
import { participantCopy, timeOfDay } from "../copy.ts";
import type { Directory, PendingReminder } from "../model.ts";
import {
  type ReminderConversation,
  reminderPath,
  reminderRepeats,
} from "../reminders.ts";
import { createPoster } from "../send.ts";
import { participant } from "../store.ts";
import { Icon } from "./Icons.tsx";

const api = createPoster();

export function Reminders({
  target,
  directory,
}: {
  target: ReminderConversation;
  directory: Directory;
}) {
  const path = reminderPath(target);
  return (
    <ConversationReminders key={path} target={target} directory={directory} />
  );
}

function ConversationReminders({
  target,
  directory,
}: {
  target: ReminderConversation;
  directory: Directory;
}) {
  const [rows, setRows] = useState<readonly PendingReminder[]>([]);
  const [error, setError] = useState(false);
  const [cancelling, setCancelling] = useState<string | null>(null);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const retry = useRef(() => {});
  const current = useRef(target);
  current.current = target;
  const cancelRequest = useRef<AbortController | null>(null);
  const cancelled = useRef(new Set<string>());
  const details = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const reminders = await api.reminders(
          current.current,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        setRows(reminders.filter((row) => !cancelled.current.has(row.id)));
        setError(false);
      } catch {
        if (!controller.signal.aborted) setError(true);
      }
      if (!controller.signal.aborted)
        timer = setTimeout(() => void refresh(), 3000);
    };
    retry.current = () => {
      clearTimeout(timer);
      void refresh();
    };
    void refresh();
    return () => {
      controller.abort();
      cancelRequest.current?.abort();
      clearTimeout(timer);
    };
  }, []);

  async function cancel(id: string) {
    if (cancelRequest.current !== null) return;
    const controller = new AbortController();
    cancelRequest.current = controller;
    setCancelling(id);
    setCancelError(null);
    const result = await api.cancelReminder(
      current.current,
      id,
      controller.signal,
    );
    if (controller.signal.aborted) return;
    cancelRequest.current = null;
    setCancelling(null);
    if (result.kind === "done") {
      if (details.current?.contains(document.activeElement))
        details.current.querySelector("summary")?.focus();
      cancelled.current.add(id);
      setRows((rows) => rows.filter((row) => row.id !== id));
    } else setCancelError(id);
  }

  if (rows.length === 0 && !error && cancelled.current.size === 0) return null;
  return (
    <details className="conversation-reminders" ref={details}>
      <summary>
        <Icon name="clock" />
        {!error || rows.length > 0
          ? `${rows.length} ${rows.length === 1 ? "reminder" : "reminders"}`
          : "Reminders unavailable"}
      </summary>
      <div className="reminder-details">
        <h3>
          {target.kind === "direct"
            ? "Pending reminders"
            : "Thread members’ reminders"}
        </h3>
        {rows.length === 0 && !error && <p>No pending reminders.</p>}
        {error && (
          <p role="alert">
            Couldn't update reminders.{" "}
            <button
              type="button"
              className="text-button"
              onClick={() => retry.current()}
            >
              Try again
            </button>
          </p>
        )}
        <ul className="reminder-list" aria-label="Pending reminders">
          {rows.map((row) => {
            const repeats = reminderRepeats(row);
            return (
              <li className="reminder-item" key={row.id}>
                <div className="reminder-copy">
                  <strong>
                    {participantCopy(participant(directory, row.owner)).name}
                  </strong>
                  <span className="reminder-body" title={row.body.text}>
                    {row.body.text}
                    {row.body.truncated ? "…" : ""}
                  </span>
                  <span>
                    {row.dueAt === null ? (
                      "Delivering now"
                    ) : (
                      <time
                        dateTime={new Date(row.dueAt).toISOString()}
                        title={new Date(row.dueAt).toLocaleString()}
                      >
                        Next {timeOfDay(row.dueAt)}
                      </time>
                    )}
                    {repeats !== null && <> · {repeats}</>}
                  </span>
                  {cancelError === row.id && (
                    <span role="alert">Couldn't cancel. Try again.</span>
                  )}
                </div>
                <button
                  type="button"
                  className="text-button"
                  disabled={cancelling !== null}
                  onClick={() => void cancel(row.id)}
                  aria-label={`Cancel reminder ${row.body.text}`}
                >
                  {cancelling === row.id ? "Cancelling…" : "Cancel"}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </details>
  );
}
