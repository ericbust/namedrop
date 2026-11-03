import { attentionReplyTo } from "./attention.ts";
import type { AddressId, Directory, ReplyTo } from "./model.ts";
import { recipients, type ThreadGroup } from "./store.ts";

// Keep local names before scoped names: a #workspace token scopes subsequent @mentions.
export function threadReplyRecipients(
  thread: ThreadGroup,
  directory: Directory,
): readonly ReplyTo[] {
  const seen = new Set<AddressId>();
  const local: ReplyTo[] = [];
  const remote: ReplyTo[] = [];
  for (const message of [...thread.parents, ...thread.replies]) {
    for (const id of [message.author, ...recipients(message)]) {
      if (id === "human" || seen.has(id)) continue;
      seen.add(id);
      const to = attentionReplyTo({ addressId: id }, directory);
      if (to === null) continue;
      (to.mentions.startsWith("#") ? remote : local).push(to);
    }
  }
  return [...local, ...remote];
}
