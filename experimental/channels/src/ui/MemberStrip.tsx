import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { presenceWord, runtimeName } from "../copy.ts";
import type { Activity, ChannelSnapshot, Member } from "../model.ts";
import { peekTarget } from "../peek.ts";
import { Avatar } from "./Avatar.tsx";
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
      return "Operator";
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
  const all = [...members.values()];
  return [
    ...all
      .filter(
        (member) =>
          member.kind.type !== "human" && member.activity !== "retired",
      )
      .sort((a, b) => order[a.activity] - order[b.activity]),
    ...all.filter((member) => member.kind.type === "human"),
  ];
}
function Chip({ member }: { member: Member }) {
  const scope = usePeek()?.scope ?? null;
  const operator = member.kind.type === "human";
  const state = operator
    ? ""
    : member.activity === "blocked"
      ? " is-blocked"
      : member.activity === "offline"
        ? " is-offline"
        : "";
  return (
    <PeekControl
      target={scope === null ? null : peekTarget(scope, member)}
      className={`member${state}`}
      title={tooltip(member)}
      fallback="span"
    >
      <Avatar
        alias={member.alias}
        type={member.kind.type}
        small
        presence={operator ? undefined : member.activity}
      />
      <span className="alias">{operator ? "You" : member.alias}</span>
      {!operator && (
        <span className="status">{presenceWord[member.activity]}</span>
      )}
    </PeekControl>
  );
}
export function MemberStrip({
  members,
}: {
  members: ChannelSnapshot["members"];
}) {
  const sorted = useMemo(() => sortedMembers(members), [members]);
  const list = useRef<HTMLUListElement>(null);
  const [width, setWidth] = useState(0);
  const [expanded, setExpanded] = useState(false);
  const [measured, setMeasured] = useState<{
    members: Member[];
    width: number;
    count: number;
  } | null>(null);
  const limit =
    !expanded && measured?.members === sorted && measured.width === width
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
      if (lines.size > 2) {
        count = Math.max(1, index - 1);
        break;
      }
    }
    setMeasured({ members: sorted, width, count });
  }, [expanded, limit, sorted, width]);
  if (sorted.length === 0)
    return (
      <div className="members">
        <p className="member-note">No members yet</p>
      </div>
    );
  const shown = limit === null ? sorted : sorted.slice(0, limit);
  return (
    <ul className="members" aria-label="Members" ref={list}>
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
  );
}
