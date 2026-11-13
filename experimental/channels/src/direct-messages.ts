import type {
  Activity,
  AddressId,
  ChannelSnapshot,
  ComposerId,
  Member,
  Message,
  SafeText,
  SpaceKey,
} from "./model.ts";
import { type Arrival, agentKey } from "./sounds.ts";

export type DirectPeer = {
  spaceKey: SpaceKey;
  addressId: AddressId;
  alias: string;
  workspaceLabel: string;
  paneLabel: SafeText | null;
  runtime: string;
  activity: Activity;
  closed: boolean;
  threadId: string | null;
  lastActivityMs: number | null;
  lastMessageId: string | null;
  lastAgentMessageId: string | null;
  attentionCount: number;
};
export type DirectHistory = {
  threadId: string | null;
  messages: readonly Message[];
  earlierOmitted: boolean;
};
export function directKey(
  peer: Pick<DirectPeer, "spaceKey" | "addressId">,
): string {
  return `${peer.spaceKey}/${peer.addressId}`;
}
export function directPath(
  peer: Pick<DirectPeer, "spaceKey" | "addressId">,
): string {
  return `/w/${peer.spaceKey}/direct-messages/${encodeURIComponent(peer.addressId)}/messages`;
}
export function directComposer(peer: DirectPeer): ComposerId {
  return { key: peer.spaceKey, thread: `dm:${directKey(peer)}` };
}
export function deriveDirectPeers(
  peers: readonly DirectPeer[],
  channels: ReadonlyMap<SpaceKey, Pick<ChannelSnapshot, "members">>,
): readonly DirectPeer[] {
  return peers.map((peer) => {
    const member = channels.get(peer.spaceKey)?.members.get(peer.addressId);
    return member === undefined || member.activity === peer.activity
      ? peer
      : { ...peer, activity: member.activity };
  });
}
export function directMember(peer: DirectPeer): Member {
  return {
    id: peer.addressId,
    alias: peer.alias,
    kind: { type: "agent", runtime: peer.runtime },
    activity: peer.activity,
    recentlyClosed: peer.closed,
    role: null,
    paneLabel: peer.paneLabel,
    subagents: [],
  };
}
export function directLabel(peer: DirectPeer): string {
  return `${peer.alias} · #${peer.workspaceLabel.replace(/^#/, "")}`;
}
export function directPeers(
  peers: readonly DirectPeer[],
  query: string,
): DirectPeer[] {
  const words = query.toLocaleLowerCase().trim().split(/\s+/);
  return peers
    .filter((peer) => {
      const text =
        `${directLabel(peer)} ${peer.paneLabel?.text ?? ""} ${peer.runtime} ${peer.activity}`.toLocaleLowerCase();
      return words.every((word) => text.includes(word));
    })
    .sort(
      (a, b) =>
        (b.lastActivityMs ?? 0) - (a.lastActivityMs ?? 0) ||
        a.workspaceLabel.localeCompare(b.workspaceLabel) ||
        a.alias.localeCompare(b.alias) ||
        directKey(a).localeCompare(directKey(b)),
    );
}
export function mergeDirectHistory(
  before: DirectHistory | undefined,
  after: DirectHistory,
  direction: "latest" | "prepend" = "latest",
): DirectHistory {
  if (!before || before.threadId !== after.threadId) return after;
  const messages = new Map(
    before.messages.map((message) => [message.id, message]),
  );
  if (
    direction === "latest" &&
    !after.messages.some((message) => messages.has(message.id))
  )
    return after;
  for (const message of after.messages) messages.set(message.id, message);
  return {
    threadId: after.threadId,
    messages: [...messages.values()].sort(
      (a, b) => a.at - b.at || a.id.localeCompare(b.id),
    ),
    earlierOmitted:
      direction === "prepend"
        ? after.earlierOmitted
        : before.earlierOmitted && after.earlierOmitted,
  };
}
export function directSelection(): string | null {
  return new URLSearchParams(location.search).get("dm");
}
// Agents with a new message to the person since the last directory read. The
// first read hears nothing: it only learns where each conversation stands.
// After it, a peer seen for the first time rings for any message it has.
export function heardDirect(
  previous: readonly DirectPeer[] | null,
  next: readonly DirectPeer[],
): Arrival[] {
  if (previous === null) return [];
  const before = new Map(
    previous.map((peer) => [directKey(peer), peer.lastAgentMessageId]),
  );
  const arrivals: Arrival[] = [];
  for (const peer of next) {
    const id = peer.lastAgentMessageId;
    if (id === null || id === (before.get(directKey(peer)) ?? null)) continue;
    const agent = agentKey(peer.addressId);
    if (agent !== null) arrivals.push({ agent, id });
  }
  return arrivals;
}
