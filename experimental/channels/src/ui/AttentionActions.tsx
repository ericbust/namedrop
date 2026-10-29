import {
  type AttentionNavigation,
  attentionConversation,
  messageIntent,
} from "../attention.ts";
import type { Attention, ChannelSnapshot, SpaceKey } from "../model.ts";
import { peekTarget } from "../peek.ts";
import { PeekControl } from "./PeekContext.tsx";

export type FocusAttention = (
  key: SpaceKey,
  navigation: AttentionNavigation,
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
    response && "addressId" in response
      ? channel.members.get(response.addressId)
      : undefined;
  const subject = item.subjectAlias ?? member?.alias;
  const navigation: AttentionNavigation | null =
    target === null
      ? null
      : item.conversation
        ? {
            kind: "context",
            target,
            response:
              response?.kind === "pane"
                ? response
                : messageIntent(item, target, false),
          }
        : { kind: "loaded", target };
  const peek = member === undefined ? null : peekTarget(channel.key, member);
  return (
    <div className="attention-actions">
      {navigation !== null && (
        <button
          type="button"
          className="text-button"
          aria-label={subject ? `Show message from ${subject}` : undefined}
          onClick={(event) =>
            onFocus(channel.key, navigation, event.currentTarget)
          }
        >
          Show message
        </button>
      )}
      {(response?.kind === "reply" ||
        (response?.kind === "message" && target !== null)) && (
        <button
          type="button"
          className="text-button"
          aria-label={member ? `Respond to ${member.alias}` : undefined}
          onClick={(event) =>
            response.kind === "reply"
              ? onFocus(
                  channel.key,
                  { kind: "context", target: response, response },
                  event.currentTarget,
                )
              : target !== null &&
                onFocus(
                  channel.key,
                  {
                    kind: "context",
                    target,
                    response: messageIntent(item, target, true),
                  },
                  event.currentTarget,
                )
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
          {item.source === "integration_registration"
            ? "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration."
            : "Open this workspace in Herdr and choose Namedrop Doctor to review this item."}
        </span>
      )}
    </div>
  );
}
