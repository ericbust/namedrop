import type { Alias, ChannelSnapshot, SendFailure, SpaceKey } from "./model.ts";
import type { DeliverResult } from "./send.ts";

export type Delivery =
  | { kind: "idle" }
  | { kind: "sending" }
  | { kind: "sent" }
  | ({ kind: "failed" } & SendFailure);
export type DeliveryEvent = { kind: "start" } | DeliverResult;

export function delivery(state: Delivery, event: DeliveryEvent): Delivery {
  switch (event.kind) {
    case "start":
      return state.kind === "idle" || state.kind === "failed"
        ? { kind: "sending" }
        : state;
    case "done":
      return state.kind === "sending" ? { kind: "sent" } : state;
    case "failed":
      return state.kind === "sending"
        ? { kind: "failed", status: event.status, message: event.message }
        : state;
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}

export type Offer = { key: SpaceKey; messageId: string; alias: Alias };
export type Deliveries = ReadonlyMap<string, { offer: Offer; state: Delivery }>;
export const idle: Delivery = { kind: "idle" };
function offerId({ key, messageId, alias }: Offer): string {
  return `${key}\n${messageId}\n${alias}`;
}
export function deliveryOf(deliveries: Deliveries, offer: Offer): Delivery {
  return deliveries.get(offerId(offer))?.state ?? idle;
}

export function createDeliveries(
  send: (offer: Offer) => Promise<DeliverResult>,
) {
  let deliveries: Deliveries = new Map();
  const listeners = new Set<() => void>();
  function commit(next: Deliveries): void {
    deliveries = next;
    for (const listener of listeners) listener();
  }
  return {
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    snapshot: (): Deliveries => deliveries,
    deliver(offer: Offer): void {
      const step = (event: DeliveryEvent): boolean => {
        const before = deliveryOf(deliveries, offer);
        const after = delivery(before, event);
        if (after === before) return false;
        commit(
          new Map(deliveries).set(offerId(offer), { offer, state: after }),
        );
        return true;
      };
      if (step({ kind: "start" })) void send(offer).then(step);
    },
    settle(
      channels: ReadonlyMap<
        SpaceKey,
        Pick<ChannelSnapshot, "members" | "messages">
      >,
    ): void {
      const open = [...deliveries].filter(([, { offer }]) => {
        const channel = channels.get(offer.key);
        return !channel?.messages
          .get(offer.messageId)
          ?.legs.some(
            (leg) => channel.members.get(leg.to)?.alias === offer.alias,
          );
      });
      if (open.length < deliveries.size) commit(new Map(open));
    },
  };
}
