import { attentionReplyTo } from "./attention.ts";
import type { Directory, SendRule, SpaceKey } from "./model.ts";
import { type SkillTarget, skillTargetKey } from "./skills.ts";
import {
  participant,
  recipients,
  type ThreadGroup,
  threadAgents,
} from "./store.ts";

export function threadSendRule(
  thread: ThreadGroup,
  directory: Directory,
): Extract<SendRule, { kind: "thread" }> {
  const to = threadAgents(thread, directory);
  let implicitRecipient: string | null = null;
  if (to.length === 1) {
    for (const message of [...thread.parents, ...thread.replies]) {
      for (const id of [message.author, ...recipients(message)]) {
        if (id === "human") continue;
        const recipient = attentionReplyTo({ addressId: id }, directory);
        if (recipient !== null) implicitRecipient = recipient.mentions;
      }
    }
  }
  return { kind: "thread", to, implicitRecipient };
}

export function threadSkillRecipients(
  thread: ThreadGroup,
  directory: Directory,
  current: SpaceKey,
): readonly SkillTarget[] {
  const targets = new Map<string, SkillTarget>();
  for (const message of [...thread.parents, ...thread.replies]) {
    for (const id of [message.author, ...recipients(message)]) {
      const who = participant(directory, id);
      if (
        who.kind !== "member" ||
        who.member.kind.type !== "agent" ||
        who.member.activity === "retired"
      )
        continue;
      const target = { key: who.home?.key ?? current, member: who.member.id };
      targets.set(skillTargetKey(target), target);
    }
  }
  return [...targets.values()];
}
