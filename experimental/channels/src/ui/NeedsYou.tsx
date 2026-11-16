import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { responderOf, responseAbsence } from "../attention.ts";
import { dismissal, type ThreadControl, threadControl } from "../control.ts";
import { attentionCopy, relativeAge, threadControlCopy } from "../copy.ts";
import type {
  Attention,
  ChannelSnapshot,
  Directory,
  SpaceKey,
} from "../model.ts";
import {
  AttentionActions,
  type ControlThread,
  type FocusAttention,
  failureCopy,
} from "./AttentionActions.tsx";
import {
  HandoffGroup,
  isHandoff,
  ReviewClears,
  type ReviewHandoffs,
  useGroupOpen,
} from "./Handoffs.tsx";
import { Icon } from "./Icons.tsx";

const SHOWN = 3;
const none: readonly Attention[] = [];
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const tick = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(tick);
  }, [active]);
  return now;
}
function useAnnouncement(key: SpaceKey, items: readonly Attention[]): string {
  const seen = useRef<{ key: SpaceKey; rows: ReadonlySet<string> } | null>(
    null,
  );
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => {
    const previous = seen.current;
    seen.current = { key, rows: new Set(items.map((item) => item.rowKey)) };
    if (previous === null) return;
    if (previous.key !== key) {
      setAnnouncement("");
      return;
    }
    const added = items.filter((item) => !previous.rows.has(item.rowKey));
    if (added.length > 0)
      setAnnouncement(
        `Needs you: ${added.map((item) => attentionCopy(item).title).join(". ")}`,
      );
  }, [key, items]);
  return announcement;
}
function Item({
  item,
  now,
  channel,
  directory,
  onFocus,
  onControl,
}: {
  item: Attention;
  now: number;
  channel: ChannelSnapshot;
  directory: Directory;
  onFocus: FocusAttention;
  onControl: ControlThread;
}) {
  const [open, setOpen] = useState(false);
  const responder = responderOf(item, channel.members);
  const absent = responseAbsence(item, directory);
  const copy = attentionCopy(item, responder, absent);
  const opened = new Date(item.openedAt);
  return (
    <li className="attention-item">
      <Icon name="alert" />
      <p className="attention-title">{copy.title}</p>
      <time
        className="attention-age"
        dateTime={Number.isFinite(opened.getTime()) ? opened.toISOString() : ""}
      >
        {relativeAge(item.openedAt, now)}
      </time>
      <div className="attention-next">
        <p className="attention-hint" title={copy.hintTitle ?? undefined}>
          {copy.hint}
        </p>
        <AttentionActions
          item={item}
          channel={channel}
          absent={absent}
          onFocus={onFocus}
          onControl={onControl}
        >
          {copy.details !== null && (
            <button
              type="button"
              className="text-button"
              aria-expanded={open}
              onClick={() => setOpen(!open)}
            >
              {open ? "Hide details" : "Details"}
            </button>
          )}
        </AttentionActions>
      </div>
      {copy.details !== null && open && (
        <p className="attention-details">{copy.details}</p>
      )}
    </li>
  );
}
const idle: ThreadControl = { kind: "idle" };
/**
 * Dismisses the cards on show, after one confirmation. `ids` are the cards
 * that are on show once `onAsk` has brought the rest of the list into view.
 * The confirmation is for the cards it counted: one that arrives while it is
 * open stays.
 */
function DismissAll({
  channel,
  ids,
  onAsk,
  onControl,
}: {
  channel: ChannelSnapshot;
  ids: readonly string[];
  onAsk: () => void;
  onControl: ControlThread;
}) {
  const [control, setControl] = useState(idle);
  const [asked, setAsked] = useState<readonly string[]>([]);
  const ask = useRef<HTMLButtonElement>(null);
  const confirm = useRef<HTMLButtonElement>(null);
  const shown = useRef(control.kind);
  const press = useRef<AbortController | null>(null);
  useEffect(() => () => press.current?.abort(), []);
  useLayoutEffect(() => {
    const before = shown.current;
    shown.current = control.kind;
    if (before === control.kind) return;
    if (control.kind === "confirming") confirm.current?.focus();
    else if (before === "confirming" && control.kind !== "pending")
      ask.current?.focus();
  });
  const busy = control.kind === "pending";
  const start = () => {
    const next = threadControl(control, { type: "start", action: "dismiss" });
    if (next === control) return;
    setControl(next);
    press.current?.abort();
    press.current = new AbortController();
    const { signal } = press.current;
    void onControl(
      channel.key,
      { kind: "attention", ids: asked, action: "dismiss" },
      signal,
    ).then((result) => {
      if (signal.aborted) return;
      // A card that arrived meanwhile stays, and so does this row.
      setControl((now) =>
        result.kind === "failed"
          ? threadControl(now, { type: "failed", status: result.status })
          : idle,
      );
    });
  };
  const cancel = () =>
    setControl((now) => threadControl(now, { type: "cancel" }));
  return (
    <>
      {control.kind === "confirming" || busy ? (
        <>
          <span className="attention-confirm">
            {threadControlCopy.confirmDismissAll(asked.length)}
          </span>
          <button
            ref={confirm}
            type="button"
            className="text-button confirm-stop"
            aria-disabled={busy}
            onClick={start}
            onKeyDown={(event) => {
              if (event.key !== "Escape") return;
              event.preventDefault();
              cancel();
            }}
          >
            {busy
              ? threadControlCopy.dismiss.pending
              : threadControlCopy.dismissCount(asked.length)}
          </button>
          <button
            type="button"
            className="text-button"
            aria-disabled={busy}
            onClick={cancel}
          >
            {threadControlCopy.cancel}
          </button>
        </>
      ) : (
        <button
          ref={ask}
          type="button"
          className="text-button"
          onClick={() => {
            setAsked(ids);
            onAsk();
            setControl((now) =>
              threadControl(now, { type: "ask", action: "dismiss" }),
            );
          }}
        >
          {threadControlCopy.dismissAll}
        </button>
      )}
      {control.kind === "failed" && (
        <span className="attention-failure" role="alert">
          {failureCopy("dismiss", control.status)}
        </span>
      )}
    </>
  );
}
export function NeedsYou({
  channel,
  directory,
  onFocus,
  onControl,
  onReview,
}: {
  channel: ChannelSnapshot;
  directory: Directory;
  onFocus: FocusAttention;
  onControl: ControlThread;
  onReview: ReviewHandoffs;
}) {
  const items = channel.health.level === "needs_you" ? channel.attention : none;
  const now = useNow(items.length > 0 || channel.reviewClears.length > 0);
  const announcement = useAnnouncement(channel.key, items);
  const [all, setAll] = useState(false);
  // Handoff warnings share one collapsible row; everything else stays in view.
  const handoffs = items.filter(isHandoff);
  const others = items.filter((item) => !isHandoff(item));
  const [handoffsOpen, toggleHandoffs] = useGroupOpen(
    channel.key,
    handoffs.length,
  );
  // Dismiss all first shows every card it is about to count. Handoff cards
  // count only while their group is open.
  const dismissable = [...others, ...(handoffsOpen ? handoffs : [])].flatMap(
    (item) => dismissal(item) ?? [],
  );
  const more = !all && others.length > SHOWN;
  return (
    <>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>
      {items.length > 0 && (
        <section className="attention" aria-label="Attention needed">
          <ul>
            {(all ? others : others.slice(0, SHOWN)).map((item) => (
              <Item
                key={item.rowKey}
                item={item}
                now={now}
                channel={channel}
                directory={directory}
                onFocus={onFocus}
                onControl={onControl}
              />
            ))}
            {handoffs.length > 0 && (
              <HandoffGroup
                items={handoffs}
                channel={channel}
                directory={directory}
                now={now}
                open={handoffsOpen}
                onToggle={toggleHandoffs}
                onFocus={onFocus}
                onControl={onControl}
                onReview={onReview}
              />
            )}
          </ul>
          {(more || dismissable.length > 1) && (
            <div className="attention-more">
              {more && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => setAll(true)}
                >
                  Show {others.length - SHOWN} more
                </button>
              )}
              {dismissable.length > 1 && (
                <DismissAll
                  channel={channel}
                  ids={dismissable}
                  onAsk={() => setAll(true)}
                  onControl={onControl}
                />
              )}
            </div>
          )}
        </section>
      )}
      <ReviewClears channel={channel} now={now} />
    </>
  );
}
