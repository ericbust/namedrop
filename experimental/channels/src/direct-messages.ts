import type {
  AddressId,
  ComposerId,
  Member,
  Message,
  SafeText,
  SpaceKey,
} from "./model.ts";

export type DirectPeer = {
  spaceKey: SpaceKey;
  addressId: AddressId;
  alias: string;
  workspaceLabel: string;
  paneLabel: SafeText | null;
  runtime: string;
  presence: "idle" | "working" | "gone";
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
export function directMember(peer: DirectPeer): Member {
  return {
    id: peer.addressId,
    alias: peer.alias,
    kind: { type: "agent", runtime: peer.runtime },
    activity:
      peer.closed || peer.presence === "gone" ? "offline" : peer.presence,
    recentlyClosed: peer.closed,
    role: null,
    paneLabel: peer.paneLabel,
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
        `${directLabel(peer)} ${peer.paneLabel?.text ?? ""} ${peer.runtime} ${peer.presence}`.toLocaleLowerCase();
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
): DirectHistory {
  if (!before || before.threadId !== after.threadId) return after;
  const messages = new Map(
    before.messages.map((message) => [message.id, message]),
  );
  for (const message of after.messages) messages.set(message.id, message);
  return {
    threadId: after.threadId,
    messages: [...messages.values()].sort(
      (a, b) => a.at - b.at || a.id.localeCompare(b.id),
    ),
    earlierOmitted: before.earlierOmitted && after.earlierOmitted,
  };
}
export function directSelection(): string | null {
  return new URLSearchParams(location.search).get("dm");
}
