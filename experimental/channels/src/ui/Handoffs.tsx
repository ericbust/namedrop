import { useEffect, useState } from "react";
import { responseAbsence } from "../attention.ts";
import { handoffCopy, relativeAge, reviewCopy } from "../copy.ts";
import type {
  AddressId,
  Attention,
  ChannelSnapshot,
  Directory,
  HandoffWarning,
  Member,
  SpaceKey,
} from "../model.ts";
import type { ReviewPost, ReviewResult } from "../send.ts";
import {
  AttentionActions,
  type ControlThread,
  type FocusAttention,
} from "./AttentionActions.tsx";
import { Icon } from "./Icons.tsx";
import { Markdown } from "./Markdown.tsx";

export type ReviewHandoffs = (
  key: SpaceKey,
  post: ReviewPost,
) => Promise<ReviewResult>;
export type Handoff = Attention & {
  coordinationId: string;
  handoff: HandoffWarning;
};
type Press =
  | { kind: "idle" }
  | { kind: "pending" }
  | { kind: "sent"; to: string; ids: readonly string[] }
  | { kind: "failed"; message: string };

export function isHandoff(item: Attention): item is Handoff {
  return (
    item.source === "coordination" &&
    typeof item.coordinationId === "string" &&
    item.handoff != null
  );
}
const mention = (alias: string) =>
  alias.startsWith("@") ? alias : `@${alias}`;
function canReview(member: Member): boolean {
  return (
    member.kind.type === "agent" &&
    member.activity !== "retired" &&
    member.activity !== "offline"
  );
}
export function reviewers(
  members: ReadonlyMap<AddressId, Member>,
): readonly Member[] {
  const agents = [...members.values()].filter(canReview);
  const claude = (member: Member) =>
    member.kind.type === "agent" && member.kind.runtime === "claude-code"
      ? 0
      : 1;
  return agents.sort((a, b) => claude(a) - claude(b));
}
// The live lead with the most of these handoffs, else the first reviewer.
export function suggestedReviewer(
  items: readonly Handoff[],
  members: ReadonlyMap<AddressId, Member>,
): AddressId | null {
  const live = reviewers(members);
  const counts = new Map<AddressId, number>();
  for (const item of items)
    if (live.some((member) => member.id === item.handoff.lead))
      counts.set(item.handoff.lead, (counts.get(item.handoff.lead) ?? 0) + 1);
  const lead = [...counts].sort((a, b) => b[1] - a[1])[0]?.[0];
  return lead ?? live[0]?.id ?? null;
}
const storageKey = (key: SpaceKey) => `namedrop:handoffs-open:${key}`;
function readOpen(key: SpaceKey): boolean | null {
  try {
    const value = localStorage.getItem(storageKey(key));
    return value === null ? null : value === "1";
  } catch {
    return null;
  }
}
// Collapsed by default once there are more than two; the operator's own
// choice wins and is kept per channel.
function useGroupOpen(key: SpaceKey, count: number) {
  const [chosen, setChosen] = useState(() => readOpen(key));
  useEffect(() => setChosen(readOpen(key)), [key]);
  const open = chosen ?? count <= 2;
  const toggle = () => {
    const next = !open;
    setChosen(next);
    try {
      localStorage.setItem(storageKey(key), next ? "1" : "0");
    } catch {}
  };
  return [open, toggle] as const;
}
function ReviewerSelect({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: readonly Member[];
  value: AddressId | null;
  onChange: (id: AddressId) => void;
}) {
  return (
    <select
      className="review-picker"
      aria-label={label}
      value={value ?? ""}
      onChange={(event) => onChange(event.currentTarget.value as AddressId)}
    >
      {options.map((member) => (
        <option key={member.id} value={member.id}>
          {mention(member.alias)}
        </option>
      ))}
    </select>
  );
}
function PressFailure({ press }: { press: Press }) {
  return press.kind === "failed" ? (
    <p className="attention-failure" role="alert">
      {press.message || reviewCopy.failed}
    </p>
  ) : null;
}
function HandoffItem({
  item,
  channel,
  directory,
  now,
  onFocus,
  onControl,
  onReview,
}: {
  item: Handoff;
  channel: ChannelSnapshot;
  directory: Directory;
  now: number;
  onFocus: FocusAttention;
  onControl: ControlThread;
  onReview: ReviewHandoffs;
}) {
  const warning = item.handoff;
  const lead = channel.members.get(warning.lead);
  const leadName = lead ? mention(lead.alias) : "the lead";
  const leadLive = lead !== undefined && canReview(lead);
  const options = reviewers(channel.members);
  const [picked, setPicked] = useState<AddressId | null>(null);
  const reviewer = leadLive ? warning.lead : (picked ?? options[0]?.id ?? null);
  const [press, setPress] = useState<Press>({ kind: "idle" });
  const [open, setOpen] = useState(false);
  const copy = handoffCopy(warning, leadName);
  const asked = warning.review;
  const askedName = asked
    ? mention(channel.members.get(asked.to)?.alias ?? "an agent")
    : null;
  async function ask() {
    if (reviewer === null || press.kind === "pending") return;
    setPress({ kind: "pending" });
    const result = await onReview(channel.key, {
      requestId: crypto.randomUUID(),
      reviewer,
      coordinationIds: [item.coordinationId],
    });
    const to = mention(channel.members.get(reviewer)?.alias ?? "the agent");
    setPress(
      result.kind === "sent"
        ? { kind: "sent", to, ids: [item.coordinationId] }
        : { kind: "failed", message: result.message },
    );
  }
  const opened = new Date(item.openedAt);
  return (
    <li
      className="attention-item handoff-item"
      data-coordination-id={item.coordinationId}
    >
      <Icon name="alert" />
      <p className="attention-title">{copy.title}</p>
      <time
        className="attention-age"
        dateTime={Number.isFinite(opened.getTime()) ? opened.toISOString() : ""}
      >
        {relativeAge(item.openedAt, now)}
      </time>
      <div className="attention-next">
        <p className="attention-hint">
          {copy.what} {copy.next}
        </p>
        <AttentionActions
          item={item}
          channel={channel}
          absent={responseAbsence(item, directory)}
          onFocus={onFocus}
          onControl={onControl}
          reviewable={reviewer !== null}
        >
          {!leadLive && options.length > 0 && (
            <ReviewerSelect
              label={reviewCopy.pick}
              options={options}
              value={reviewer}
              onChange={setPicked}
            />
          )}
          {reviewer !== null && (
            <button
              type="button"
              className="text-button review-ask"
              aria-disabled={press.kind === "pending"}
              onClick={() => void ask()}
            >
              {press.kind === "pending"
                ? reviewCopy.pending
                : leadLive
                  ? reviewCopy.ask(leadName)
                  : reviewCopy.askPicked}
            </button>
          )}
          <button
            type="button"
            className="text-button"
            aria-expanded={open}
            onClick={() => setOpen(!open)}
          >
            {open ? "Hide details" : "Details"}
          </button>
        </AttentionActions>
      </div>
      {reviewer === null && (
        <p className="review-status">{reviewCopy.noAgent}</p>
      )}
      {asked && askedName && (
        <p className="review-status" role="status">
          <Icon name="clock" />
          {reviewCopy.sent(askedName)} · {relativeAge(asked.requestedAt, now)}
        </p>
      )}
      <PressFailure press={press} />
      {open && (
        <div className="attention-details handoff-details">
          <p>{item.summary.text}</p>
          {warning.report && (
            <div className="handoff-report">
              <p className="handoff-report-label">Report</p>
              <div className="body">
                <Markdown source={warning.report.text} />
              </div>
            </div>
          )}
        </div>
      )}
    </li>
  );
}
export function HandoffGroup({
  items,
  channel,
  directory,
  now,
  onFocus,
  onControl,
  onReview,
}: {
  items: readonly Handoff[];
  channel: ChannelSnapshot;
  directory: Directory;
  now: number;
  onFocus: FocusAttention;
  onControl: ControlThread;
  onReview: ReviewHandoffs;
}) {
  const [open, toggle] = useGroupOpen(channel.key, items.length);
  const options = reviewers(channel.members);
  const [picked, setPicked] = useState<AddressId | null>(null);
  const reviewer =
    picked !== null && options.some((member) => member.id === picked)
      ? picked
      : suggestedReviewer(items, channel.members);
  const [press, setPress] = useState<Press>({ kind: "idle" });
  async function reviewAll() {
    if (reviewer === null || press.kind === "pending") return;
    setPress({ kind: "pending" });
    const result = await onReview(channel.key, {
      requestId: crypto.randomUUID(),
      reviewer,
      coordinationIds: items.map((item) => item.coordinationId),
    });
    const to = mention(channel.members.get(reviewer)?.alias ?? "the agent");
    setPress(
      result.kind === "sent"
        ? { kind: "sent", to, ids: items.map((item) => item.coordinationId) }
        : { kind: "failed", message: result.message },
    );
  }
  const cleared =
    press.kind === "sent"
      ? press.ids.filter(
          (id) => !items.some((item) => item.coordinationId === id),
        ).length
      : 0;
  return (
    <li className="handoff-group">
      <div className="handoff-head">
        <button
          type="button"
          className="handoff-toggle"
          aria-expanded={open}
          onClick={toggle}
        >
          <Icon name="chevron" className={open ? "turned" : undefined} />
          <span className="attention-title">
            {reviewCopy.group(items.length)}
          </span>
        </button>
        <div className="handoff-all">
          {reviewer !== null && (
            <button
              type="button"
              className="text-button review-all"
              aria-disabled={press.kind === "pending"}
              onClick={() => void reviewAll()}
            >
              {press.kind === "pending" ? reviewCopy.pending : reviewCopy.all}
            </button>
          )}
          {options.length > 0 && (
            <>
              <span className="handoff-with">{reviewCopy.with}</span>
              <ReviewerSelect
                label={`${reviewCopy.pick} for ${reviewCopy.all}`}
                options={options}
                value={reviewer}
                onChange={setPicked}
              />
            </>
          )}
        </div>
        {press.kind === "sent" ? (
          <p className="review-status" role="status">
            {reviewCopy.sentAll(press.ids.length, press.to)}
            {reviewCopy.progress(cleared, press.ids.length)}
          </p>
        ) : (
          <PressFailure press={press} />
        )}
      </div>
      {open && (
        <ul>
          {items.map((item) => (
            <HandoffItem
              key={item.rowKey}
              item={item}
              channel={channel}
              directory={directory}
              now={now}
              onFocus={onFocus}
              onControl={onControl}
              onReview={onReview}
            />
          ))}
        </ul>
      )}
    </li>
  );
}
export function ReviewClears({
  channel,
  now,
}: {
  channel: ChannelSnapshot;
  now: number;
}) {
  const [dismissed, setDismissed] = useState(0);
  const shown = channel.reviewClears.filter((clear) => clear.at > dismissed);
  if (shown.length === 0) return null;
  return (
    <section className="review-clears" aria-label="Review results">
      <ul>
        {shown.map((clear) => (
          <li key={`${clear.coordinationId}-${clear.at}`}>
            <Icon name="check" />
            <span>
              “{clear.deliverable.text}” ·{" "}
              {reviewCopy.cleared(
                mention(channel.members.get(clear.by)?.alias ?? "an agent"),
              )}
            </span>
            <time>{relativeAge(clear.at, now)}</time>
          </li>
        ))}
      </ul>
      <button
        type="button"
        className="text-button"
        onClick={() =>
          setDismissed(Math.max(...shown.map((clear) => clear.at)))
        }
      >
        {reviewCopy.dismiss}
      </button>
    </section>
  );
}
