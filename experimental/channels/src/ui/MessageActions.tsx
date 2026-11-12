import { createContext, type ReactNode } from "react";
import type { ImageUpload } from "../images.ts";
import type { Message, SafeText } from "../model.ts";
import { type DeliveryAccess, DeliveryContext } from "./DeliveryContext.ts";
import { ImageUploadContext } from "./ImageUploadContext.ts";

export type MessageActions = Readonly<{
  fullBody(messageId: string): Promise<SafeText | null>;
  reply: ((message: Message, button: HTMLButtonElement) => void) | null;
}>;
export const ThreadConversationContext = createContext(false);

export const MessageActionsContext = createContext<MessageActions | null>(null);

export function MessageContexts({
  deliveries,
  actions,
  uploadImage = null,
  children,
}: {
  deliveries: DeliveryAccess | null;
  actions: MessageActions | null;
  uploadImage?: ((file: Blob) => Promise<ImageUpload>) | null;
  children: ReactNode;
}) {
  return (
    <DeliveryContext value={deliveries}>
      <MessageActionsContext value={actions}>
        <ImageUploadContext value={uploadImage}>{children}</ImageUploadContext>
      </MessageActionsContext>
    </DeliveryContext>
  );
}
