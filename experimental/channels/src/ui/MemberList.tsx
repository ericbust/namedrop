import type { Activity, Channel, Health, Member } from "../model.ts";

const presence = {
  working: "Working",
  waiting: "Waiting",
  blocked: "Blocked",
  idle: "Idle",
  offline: "Offline",
  retired: "Retired",
} satisfies Record<Activity, string>;
function description(member: Member): string {
  if (member.role?.text) return member.role.text;
  switch (member.kind.type) {
    case "human":
      return "Operator";
    case "terminal":
      return "Terminal";
    case "agent":
      return member.kind.runtime || "Agent";
    default: {
      const exhaustive: never = member.kind;
      return exhaustive;
    }
  }
}
function healthLabel(health: Health): string {
  switch (health.level) {
    case "calm":
      return "All quiet";
    case "busy":
      return `${health.working} working · ${health.inFlight} in flight`;
    case "needs_you":
      return "Needs you";
    case "paused":
      return "Paused";
    case "inactive":
      return "Not active";
    default: {
      const exhaustive: never = health;
      return exhaustive;
    }
  }
}
export function MemberList({
  channel,
  name,
}: {
  channel: Channel;
  name: string;
}) {
  return (
    <section
      className="channel-content"
      aria-labelledby="channel-heading"
      data-selected-channel={channel.key}
    >
      <header className="channel-header">
        <div>
          <h1 id="channel-heading">{name}</h1>
          <p>
            {channel.members.size}{" "}
            {channel.members.size === 1 ? "member" : "members"}
            <span className="text-separator">·</span>
            {healthLabel(channel.health)}
          </p>
        </div>
        <span className="address-label" title="Workspace mention">
          {channel.addressableLabel === null
            ? "Not addressable"
            : `#${channel.addressableLabel}`}
        </span>
      </header>
      {channel.health.level === "needs_you" && (
        <section className="attention-summary" aria-label="Attention needed">
          <h2>
            <span
              className="presence-dot"
              data-activity="blocked"
              aria-hidden="true"
            />
            Needs you
          </h2>
          <ul>
            {channel.attention.map((item) => (
              <li key={`${item.source}-${item.openedAt}-${item.summary.text}`}>
                {item.summary.text}
                {item.summary.truncated ? "…" : ""}
              </li>
            ))}
          </ul>
        </section>
      )}
      <div className="members-heading">
        <h2>Members</h2>
        <span>Status</span>
      </div>
      {channel.members.size === 0 ? (
        <div className="empty-members">
          <h3>No members yet</h3>
          <p>Name an agent or terminal in this workspace to see it here.</p>
        </div>
      ) : (
        <ul className="member-list" aria-label="Members">
          {[...channel.members.values()].map((member) => (
            <li
              className="member-row"
              key={member.id}
              data-address-id={member.id}
              data-activity={member.activity}
            >
              <div className="member-identity">
                <span className="member-alias">{member.alias}</span>
                <span className="member-description">
                  {description(member)}
                  {member.role?.truncated ? "…" : ""}
                  {member.paneLabel?.text
                    ? ` · ${member.paneLabel.text}${member.paneLabel.truncated ? "…" : ""}`
                    : ""}
                </span>
              </div>
              <span className="member-status">
                <span
                  className="presence-dot"
                  data-activity={member.activity}
                  aria-hidden="true"
                />
                {presence[member.activity]}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
