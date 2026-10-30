import { useEffect, useRef, useState } from "react";
import { attentionCopy, relativeAge } from "../copy.ts";
import type { Attention, ChannelSnapshot, SpaceKey } from "../model.ts";
import {
  AttentionActions,
  type ControlThread,
  type FocusAttention,
} from "./AttentionActions.tsx";
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
  onFocus,
  onControl,
}: {
  item: Attention;
  now: number;
  channel: ChannelSnapshot;
  onFocus: FocusAttention;
  onControl: ControlThread;
}) {
  const [open, setOpen] = useState(false);
  const copy = attentionCopy(item);
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
      <p className="attention-hint">{copy.hint}</p>
      <AttentionActions
        item={item}
        channel={channel}
        onFocus={onFocus}
        onControl={onControl}
      />
      {copy.details !== null && (
        <div className="attention-actions">
          <button
            type="button"
            className="text-button"
            aria-expanded={open}
            onClick={() => setOpen(!open)}
          >
            {open ? "Hide details" : "Details"}
          </button>
        </div>
      )}
      {copy.details !== null && open && (
        <p className="attention-details">{copy.details}</p>
      )}
    </li>
  );
}
export function NeedsYou({
  channel,
  onFocus,
  onControl,
}: {
  channel: ChannelSnapshot;
  onFocus: FocusAttention;
  onControl: ControlThread;
}) {
  const items = channel.health.level === "needs_you" ? channel.attention : none;
  const now = useNow(items.length > 0);
  const announcement = useAnnouncement(channel.key, items);
  const [all, setAll] = useState(false);
  return (
    <>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>
      {items.length > 0 && (
        <section className="attention" aria-label="Attention needed">
          <ul>
            {(all ? items : items.slice(0, SHOWN)).map((item) => (
              <Item
                key={item.rowKey}
                item={item}
                now={now}
                channel={channel}
                onFocus={onFocus}
                onControl={onControl}
              />
            ))}
          </ul>
          {!all && items.length > SHOWN && (
            <div className="attention-more">
              <button
                type="button"
                className="text-button"
                onClick={() => setAll(true)}
              >
                Show {items.length - SHOWN} more
              </button>
            </div>
          )}
        </section>
      )}
    </>
  );
}
