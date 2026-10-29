import { attentionConversation, type ReplyIntent } from "../attention.ts";
import type {
  Attention,
  ChannelSnapshot,
  Conversation,
  SpaceKey,
} from "../model.ts";
import { peekTarget } from "../peek.ts";
import { PeekControl } from "./PeekContext.tsx";

export type FocusAttention = (
  key: SpaceKey,
  target: Conversation,
  reply: ReplyIntent | null,
  button: HTMLButtonElement,
) => void;
export function AttentionActions({
  item,
  channel,
  onFocus,
}: {
  item: Attention;
  channel: Pick<ChannelSnapshot, "key" | "members" | "messages">;
  onFocus: FocusAttention;
}) {
  const target = attentionConversation(item, channel);
  const response = item.response;
  const member =
    response?.kind === "pane"
      ? channel.members.get(response.addressId)
      : undefined;
  const peek = member === undefined ? null : peekTarget(channel.key, member);
  return (
    <div className="attention-actions">
      {target !== null && (
        <button
          type="button"
          className="text-button"
          onClick={(event) =>
            onFocus(channel.key, target, null, event.currentTarget)
          }
        >
          Show message
        </button>
      )}
      {response?.kind === "reply" && (
        <button
          type="button"
          className="text-button"
          onClick={(event) =>
            onFocus(channel.key, response, response, event.currentTarget)
          }
        >
          Respond
        </button>
      )}
      {response?.kind === "pane" && (
        <>
          <span>
            Respond in{" "}
            {member?.alias ?? item.subjectAlias ?? response.addressId}
            {member?.paneLabel ? ` · ${member.paneLabel.text}` : ""} in Herdr.
          </span>
          {peek !== null && (
            <PeekControl target={peek} className="text-button" fallback="none">
              Peek
            </PeekControl>
          )}
        </>
      )}
      {response?.kind === "local" && (
        <span>
          Open this workspace in Herdr and choose Namedrop setup from the
          command palette to repair the integration.
        </span>
      )}
    </div>
  );
}
