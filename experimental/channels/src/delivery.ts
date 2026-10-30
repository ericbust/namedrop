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
export const idle: Delivery = { kind: "idle" };
// `reached`: a frame, or a deliver answer's channel object, shows the post
// reached the agent. `held` while a sent press still has its hold to run.
export type DeliveryEvent =
  | { kind: "start" }
  | DeliverResult
  | { kind: "reached"; held: boolean };

// A press is confirmed by its answer or by the line naming the agent,
// whichever comes first. It then reads Sent until its hold is over and the
// line names the agent, and a later answer, even a failure, changes nothing.
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
    case "reached":
      switch (state.kind) {
        case "sending":
          return { kind: "sent" };
        case "sent":
          return event.held ? state : idle;
        case "failed":
          return idle;
        case "idle":
          return state;
        default: {
          const exhaustive: never = state;
          return exhaustive;
        }
      }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}

// How long a confirmed press reads "Sent to @codex" at least, so the
// confirmation shows whichever of the answer and the frame came first.
export const SENT_HOLD_MS = 1500;
export type Offer = { key: SpaceKey; messageId: string; alias: Alias };
export type Change = { offer: Offer; state: Delivery };
export type Deliveries = ReadonlyMap<string, Change>;
type Channels = ReadonlyMap<
  SpaceKey,
  Pick<ChannelSnapshot, "members" | "messages">
>;
function offerId({ key, messageId, alias }: Offer): string {
  return `${key}\n${messageId}\n${alias}`;
}
export function deliveryOf(deliveries: Deliveries, offer: Offer): Delivery {
  return deliveries.get(offerId(offer))?.state ?? idle;
}
function reachedIn(channels: Channels, offer: Offer): boolean {
  const channel = channels.get(offer.key);
  const post = channel?.messages.get(offer.messageId);
  return (
    post !== undefined &&
    (post.routing?.to.includes(offer.alias) === true ||
      post.legs.some(
        (leg) => channel?.members.get(leg.to)?.alias === offer.alias,
      ))
  );
}

export function createDeliveries(
  send: (offer: Offer) => Promise<ReadonlyMap<Alias, DeliverResult>>,
) {
  let deliveries: Deliveries = new Map();
  let latest: Change | null = null;
  // What the page last showed, for a hold that ends between frames.
  let shown: Channels = new Map();
  // One timer per sent press. While it runs, the press keeps reading Sent.
  const holds = new Map<string, ReturnType<typeof setTimeout>>();
  const listeners = new Set<() => void>();
  function apply(moves: readonly (readonly [Offer, DeliveryEvent])[]): boolean {
    let next: Map<string, Change> | null = null;
    for (const [offer, event] of moves) {
      const id = offerId(offer);
      const before = deliveryOf(next ?? deliveries, offer);
      const after = delivery(before, event);
      if (after === before) continue;
      next ??= new Map(deliveries);
      if (after.kind === "idle") {
        next.delete(id);
        continue;
      }
      latest = { offer, state: after };
      next.set(id, latest);
      if (after.kind === "sent") hold(id);
    }
    if (next === null) return false;
    deliveries = next;
    for (const listener of listeners) listener();
    return true;
  }
  function hold(id: string): void {
    holds.set(
      id,
      setTimeout(() => {
        holds.delete(id);
        settle(shown);
      }, SENT_HOLD_MS),
    );
  }
  function settle(channels: Channels): void {
    shown = channels;
    apply(
      [...deliveries.values()].flatMap(({ offer }) =>
        reachedIn(channels, offer)
          ? [[offer, { kind: "reached", held: holds.has(offerId(offer)) }]]
          : [],
      ),
    );
  }
  return {
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    snapshot: (): Deliveries => deliveries,
    latest: (): Change | null => latest,
    deliver(offer: Offer): void {
      if (apply([[offer, { kind: "start" }]]))
        void send(offer).then((results) =>
          apply(
            [...results].map(([alias, result]) => [
              { ...offer, alias },
              result,
            ]),
          ),
        );
    },
    settle,
    dispose(): void {
      for (const timer of holds.values()) clearTimeout(timer);
      holds.clear();
    },
  };
}
