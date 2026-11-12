import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createCopy, presenceWord, runtimeName } from "../copy.ts";
import { takenNames } from "../create.ts";
import type { Activity, ChannelSnapshot, Member } from "../model.ts";
import { peekTarget } from "../peek.ts";
import { AddAgentForm, type AddAgentRequest } from "./AddAgent.tsx";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";
import { useShortWindow } from "./measure.ts";
import { PeekControl, usePeek } from "./PeekContext.tsx";

const order = {
  blocked: 0,
  working: 1,
  waiting: 2,
  idle: 3,
  offline: 4,
  unknown: 5,
  retired: 6,
} satisfies Record<Activity, number>;
function kindName(member: Member): string {
  switch (member.kind.type) {
    case "agent":
      return runtimeName(member.kind.runtime);
    case "terminal":
      return "Terminal";
    case "human":
      return "You";
    case "unknown":
      return "Member";
    default: {
      const exhaustive: never = member.kind;
      return exhaustive;
    }
  }
}
function tooltip(member: Member): string {
  return [kindName(member), member.role, member.paneLabel]
    .map((part) =>
      part === null || typeof part === "string"
        ? part
        : part.text && `${part.text}${part.truncated ? "…" : ""}`,
    )
    .filter(Boolean)
    .join(" · ");
}
export function sortedMembers(members: ChannelSnapshot["members"]): Member[] {
  const all = [...members.values()].filter(
    (member) => member.activity !== "retired" || member.recentlyClosed,
  );
  return [
    ...all
      .filter((member) => member.kind.type !== "human")
      .sort((a, b) => order[a.activity] - order[b.activity]),
    ...all.filter((member) => member.kind.type === "human"),
  ];
}
function Chip({ member }: { member: Member }) {
  const scope = usePeek()?.scope ?? null;
  const detailsId = useId();
  const details = useRef<HTMLDivElement>(null);
  const entry = useRef<HTMLFieldSetElement>(null);
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(Date.now);
  const count = member.subagents.length;
  useLayoutEffect(() => {
    const popup = details.current;
    const anchor = entry.current;
    if (!popup || !anchor || !open || count === 0) return;
    popup.showPopover();
    const rect = anchor.getBoundingClientRect();
    const bounds = popup.getBoundingClientRect();
    popup.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - bounds.width - 8))}px`;
    popup.style.top = `${Math.max(8, Math.min(rect.bottom, innerHeight - bounds.height - 8))}px`;
    return () => {
      if (popup.isConnected) popup.hidePopover();
    };
  }, [open, count]);
  useEffect(() => {
    if (!open || count === 0) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    const dismiss = () => setOpen(false);
    const onEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        dismiss();
      }
    };
    addEventListener("keydown", onEscape, true);
    addEventListener("resize", dismiss);
    const onScroll = (event: Event) => {
      if (
        event.target instanceof Node &&
        details.current?.contains(event.target)
      )
        return;
      dismiss();
    };
    addEventListener("scroll", onScroll, true);
    return () => {
      clearInterval(timer);
      removeEventListener("keydown", onEscape, true);
      removeEventListener("resize", dismiss);
      removeEventListener("scroll", onScroll, true);
    };
  }, [open, count]);
  const operator = member.kind.type === "human";
  const state = operator
    ? ""
    : member.activity === "blocked"
      ? " is-blocked"
      : member.activity === "offline"
        ? " is-offline"
        : member.activity === "retired"
          ? " is-retired"
          : "";
  return (
    <fieldset
      className="member-entry"
      ref={entry}
      onMouseEnter={() => setOpen(count > 0)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(count > 0)}
      onBlur={() => setOpen(false)}
    >
      <PeekControl
        target={scope === null ? null : peekTarget(scope, member)}
        className={`member${state}`}
        title={count === 0 ? tooltip(member) : undefined}
        describedBy={count > 0 ? detailsId : undefined}
        fallback="span"
      >
        <Avatar
          alias={member.alias}
          type={member.kind.type}
          small
          presence={operator ? undefined : member.activity}
        />
        <span className="alias">{operator ? "You" : member.alias}</span>
        {count > 0 && (
          <span className="subagent-count">
            · {count} {count === 1 ? "subagent" : "subagents"}
          </span>
        )}
        {!operator && (
          <span className="status">{presenceWord[member.activity]}</span>
        )}
      </PeekControl>
      {count > 0 && (
        <div
          className="member-subagents"
          id={detailsId}
          ref={details}
          popover="manual"
          role="tooltip"
        >
          <p>
            {member.alias} · {count} {count === 1 ? "subagent" : "subagents"}
          </p>
          <p className="subagent-context">{tooltip(member)}</p>
          <ul aria-label={`Subagents of ${member.alias}`}>
            {member.subagents.map((agent) => {
              const seconds = Math.max(
                0,
                Math.floor((now - agent.startedAt) / 1000),
              );
              const elapsed =
                seconds < 60
                  ? `${seconds}s`
                  : seconds < 3600
                    ? `${Math.floor(seconds / 60)}m ${seconds % 60}s`
                    : `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
              return (
                <li key={agent.id}>
                  <span className="subagent-kind">
                    {agent.kind?.text || "Subagent"}
                    {agent.kind?.truncated && "…"}
                  </span>
                  <span className="subagent-elapsed">{elapsed}</span>
                  {agent.description?.text && (
                    <span className="subagent-description">
                      {agent.description.text}
                      {agent.description.truncated && "…"}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </fieldset>
  );
}
// `addAgent` is null while the channel can't take a new agent.
export function MemberStrip({
  members,
  addAgent,
}: {
  members: ChannelSnapshot["members"];
  addAgent: AddAgentRequest | null;
}) {
  const sorted = useMemo(() => sortedMembers(members), [members]);
  const [adding, setAdding] = useState(false);
  const addButton = useRef<HTMLButtonElement>(null);
  const formId = useId();
  const taken = useMemo(() => takenNames(members.values()), [members]);
  const closeForm = () => {
    setAdding(false);
    addButton.current?.focus();
  };
  // A channel that pauses drops the form, so resuming never brings back a stale one.
  const canAdd = addAgent !== null;
  useEffect(() => {
    if (!canAdd) setAdding(false);
  }, [canAdd]);
  const list = useRef<HTMLUListElement>(null);
  const [width, setWidth] = useState(0);
  const [expanded, setExpanded] = useState(false);
  const rows = useShortWindow() ? 1 : 2;
  const [measured, setMeasured] = useState<{
    members: Member[];
    width: number;
    rows: number;
    count: number;
  } | null>(null);
  const limit =
    !expanded &&
    measured?.members === sorted &&
    measured.width === width &&
    measured.rows === rows
      ? measured.count
      : null;
  useEffect(() => {
    const element = list.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const element = list.current;
    if (expanded || limit !== null || !element) return;
    const lines = new Set<number>();
    let count = sorted.length;
    for (const [index, item] of [...element.children].entries()) {
      if (!(item instanceof HTMLElement)) continue;
      lines.add(item.offsetTop);
      if (lines.size > rows) {
        count = Math.max(1, index - 1);
        break;
      }
    }
    setMeasured({ members: sorted, width, rows, count });
  }, [expanded, limit, rows, sorted, width]);
  const shown = limit === null ? sorted : sorted.slice(0, limit);
  const open = adding && canAdd;
  return (
    <>
      <div className="members">
        {sorted.length === 0 ? (
          <p className="member-note">No members yet</p>
        ) : (
          <ul className="member-list" aria-label="Members" ref={list}>
            {shown.map((member) => (
              <li key={member.id} data-address-id={member.id}>
                <Chip member={member} />
              </li>
            ))}
            {shown.length < sorted.length && (
              <li>
                <button
                  type="button"
                  className="member more"
                  aria-expanded="false"
                  onClick={() => setExpanded(true)}
                >
                  +{sorted.length - shown.length} more
                </button>
              </li>
            )}
          </ul>
        )}
        {addAgent !== null && (
          <button
            ref={addButton}
            type="button"
            className="member member-add"
            aria-expanded={open}
            aria-controls={open ? formId : undefined}
            onClick={() => (open ? closeForm() : setAdding(true))}
          >
            <Icon name="plus" className="icon-14" />
            {createCopy.addAgent}
          </button>
        )}
      </div>
      {open && (
        <AddAgentForm
          id={formId}
          taken={taken}
          add={addAgent}
          onClose={closeForm}
        />
      )}
    </>
  );
}
