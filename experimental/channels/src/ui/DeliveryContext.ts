import { createContext } from "react";
import type { Delivery } from "../delivery.ts";
import type { Alias, Message } from "../model.ts";

export type DeliveryAccess = Readonly<{
  of(messageId: string, alias: Alias): Delivery;
  deliver(post: Message, alias: Alias): void;
}>;
export const DeliveryContext = createContext<DeliveryAccess | null>(null);
