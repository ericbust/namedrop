import { healthCopy } from "../copy.ts";
import type { ChannelSnapshot } from "../model.ts";
import { Icon } from "./Icons.tsx";

export function ChannelHeader({
  channel,
  suffix,
}: {
  channel: ChannelSnapshot;
  suffix: string | null;
}) {
  const health = healthCopy(channel.health);
  return (
    <header className="channel-header" data-selected-channel={channel.key}>
      <Icon name="hash" className="channel-icon" />
      <h1 id="channel-heading">
        {channel.displayName}
        {suffix !== null && <span className="name-suffix"> · {suffix}</span>}
      </h1>
      {channel.addressableLabel === null ? (
        <span
          className="token"
          title="Two workspaces share this label, or it has none. Rename one in Herdr to mention it."
        >
          No mention label
        </span>
      ) : (
        <span className="token" title="Mention this channel from any workspace">
          #{channel.addressableLabel}
        </span>
      )}
      {health !== null && (
        <span className="health" data-tone={health.tone}>
          {health.tone === "held" && <Icon name="pause" />}
          {health.text}
        </span>
      )}
    </header>
  );
}
