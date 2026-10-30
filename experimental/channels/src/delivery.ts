import type {
  Alias,
  ChannelSnapshot,
  DeliverFailure,
  DeliverResult,
  SpaceKey,
} from "./model.ts";

export type Delivery =
  | { kind: "idle" }
  | { kind: "sending" }
  | { kind: "sent" }
  | ({ kind: "failed" } & DeliverFailure);
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
      return state.kind === "sending" ? event : state;
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
  send: (offer: Offer) => Promise<ReadonlyMap<Alias, DeliverResult>>,
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
      const step = (alias: Alias, event: DeliveryEvent): boolean => {
        const target = { ...offer, alias };
        const before = deliveryOf(deliveries, target);
        const after = delivery(before, event);
        if (after === before) return false;
        commit(
          new Map(deliveries).set(offerId(target), {
            offer: target,
            state: after,
          }),
        );
        return true;
      };
      if (step(offer.alias, { kind: "start" }))
        void send(offer).then((results) => {
          for (const [alias, result] of results) step(alias, result);
        });
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
