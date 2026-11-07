import { createContext, type ReactNode } from "react";
import type { Message, SafeText } from "../model.ts";
import { type DeliveryAccess, DeliveryContext } from "./DeliveryContext.ts";

export type MessageActions = Readonly<{
  fullBody(messageId: string): Promise<SafeText | null>;
  reply: ((message: Message, button: HTMLButtonElement) => void) | null;
}>;
export const ThreadConversationContext = createContext(false);

export const MessageActionsContext = createContext<MessageActions | null>(null);

export function MessageContexts({
  deliveries,
  actions,
  children,
}: {
  deliveries: DeliveryAccess | null;
  actions: MessageActions | null;
  children: ReactNode;
}) {
  return (
    <DeliveryContext value={deliveries}>
      <MessageActionsContext value={actions}>{children}</MessageActionsContext>
    </DeliveryContext>
  );
}
