import { memo } from "react";
import {
  type ActivityList,
  type ActivityRow,
  activitySummary,
  lineText,
} from "../activity.ts";
import { activityCopy, presenceWord } from "../copy.ts";
import type { Home, SpaceKey } from "../model.ts";
import type { PeekKey } from "../peek.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";
import { PeekControl } from "./PeekContext.tsx";

const Row = memo(function Row({
  row,
  homes,
  open,
  onJump,
}: {
  row: ActivityRow;
  homes: ReadonlyMap<string, Home>;
  open: boolean;
  onJump: (key: SpaceKey) => void;
}) {
  const { member, line, channel } = row;
  const text = lineText(line, homes);
  const held = line.kind === "mail" && line.held;
  return (
    <li className="activity-item">
      <PeekControl
        target={row.peek}
        className={`activity-row${text === "" ? " is-single" : ""}`}
        expanded={open}
        fallback="span"
      >
        <Avatar
          alias={member.alias}
          type={member.kind.type}
          presence={member.activity}
        />
        <span className="activity-alias">
          {member.alias}
          <span className="sr-only"> {presenceWord[member.activity]}</span>
        </span>
        {text !== "" && (
          <span
            className={`activity-line${line.kind === "attention" ? " is-attention" : ""}`}
          >
            <span className="activity-text" title={text}>
              {text}
            </span>
            {held && (
              <span className="activity-held">
                <Icon name="pause" className="icon-12" />
                {activityCopy.held}
              </span>
            )}
          </span>
        )}
        {row.since !== null && (
          <span className="activity-since">
            {activityCopy.since(row.since)}
          </span>
        )}
      </PeekControl>
      <button
        type="button"
        className="activity-channel"
        title={channel.suffix ?? undefined}
        onClick={() => onJump(channel.key)}
      >
        #{channel.displayName}
      </button>
    </li>
  );
});

export function ActivityView({
  list,
  homes,
  open,
  onJump,
}: {
  list: ActivityList;
  homes: ReadonlyMap<string, Home>;
  open: PeekKey | null;
  onJump: (key: SpaceKey) => void;
}) {
  const summary = activitySummary(list);
  return (
    <>
      <header className="channel-header">
        <Icon name="pulse" className="channel-icon" />
        <h1>{activityCopy.title}</h1>
        {summary !== "" && <span className="health">{summary}</span>}
      </header>
      <section
        className="activity-list"
        aria-label={activityCopy.title}
        // biome-ignore lint/a11y/noNoninteractiveTabindex: Keyboard scrolling requires focus. https://www.w3.org/WAI/standards-guidelines/act/rules/0ssw9k/
        tabIndex={0}
      >
        {list.groups.length === 0 ? (
          <div className="empty">
            <Icon name="pulse" />
            <h2>{activityCopy.idleTitle}</h2>
            <p>{activityCopy.idleBody}</p>
          </div>
        ) : (
          list.groups.map((group) => (
            <div key={group.kind} className="activity-group">
              <h2 className="nav-label">
                {activityCopy.groups[group.kind]}
                <span className="count">{group.rows.length}</span>
              </h2>
              <ul>
                {group.rows.map((row) => (
                  <Row
                    key={row.key}
                    row={row}
                    homes={homes}
                    open={row.key === open}
                    onJump={onJump}
                  />
                ))}
              </ul>
            </div>
          ))
        )}
      </section>
    </>
  );
}
