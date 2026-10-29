import type { Member, Participant, PeekTarget, SpaceKey } from "./model.ts";

export type PeekKey = string & { readonly __brand: "PeekKey" };
export function peekKey(
  target: Pick<PeekTarget, "channel" | "member">,
): PeekKey {
  return `${target.channel}/${target.member}` as PeekKey;
}
export function samePeek(a: PeekTarget, b: PeekTarget): boolean {
  return a.channel === b.channel && a.member === b.member;
}
export function peekTarget(
  channel: SpaceKey,
  member: Member,
): PeekTarget | null {
  const { type } = member.kind;
  if (type === "human") return null;
  return {
    channel,
    member: member.id,
    alias: member.alias,
    kind: type,
  } as PeekTarget;
}
// An envoy that resolved to its home agent is read in its home channel under its
// origin id, because the server answers 410 for the envoy's own id.
export function peekTargetOf(
  who: Participant,
  scope: SpaceKey | null,
): PeekTarget | null {
  if (who.kind !== "member") return null;
  const channel = who.home === null ? scope : who.home.key;
  return channel === null ? null : peekTarget(channel, who.member);
}
