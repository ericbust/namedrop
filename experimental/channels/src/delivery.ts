import type { Alias, ChannelSnapshot, SendFailure, SpaceKey } from "./model.ts";
import type { DeliverResult } from "./send.ts";

// Send anyway and Send to: one offer's request to deliver a stored post to an agent.
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
export const noDeliveries: Deliveries = new Map();
const idle: Delivery = { kind: "idle" };
function offerId({ key, messageId, alias }: Offer): string {
  return `${key}\n${messageId}\n${alias}`;
}

export function deliveryOf(deliveries: Deliveries, offer: Offer): Delivery {
  return deliveries.get(offerId(offer))?.state ?? idle;
}
export function stepDelivery(
  deliveries: Deliveries,
  offer: Offer,
  event: DeliveryEvent,
): Deliveries {
  const before = deliveryOf(deliveries, offer);
  const after = delivery(before, event);
  return after === before
    ? deliveries
    : new Map(deliveries).set(offerId(offer), { offer, state: after });
}
// An offer's state ends once a frame shows its post reached that agent, which
// also takes the offer off the route line.
export function settleDeliveries(
  deliveries: Deliveries,
  channels: ReadonlyMap<
    SpaceKey,
    Pick<ChannelSnapshot, "members" | "messages">
  >,
): Deliveries {
  let settled: Map<string, { offer: Offer; state: Delivery }> | undefined;
  for (const [id, { offer }] of deliveries) {
    const channel = channels.get(offer.key);
    const reached = channel?.messages
      .get(offer.messageId)
      ?.legs.some((leg) => channel.members.get(leg.to)?.alias === offer.alias);
    if (!reached) continue;
    settled ??= new Map(deliveries);
    settled.delete(id);
  }
  return settled ?? deliveries;
}
