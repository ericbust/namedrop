import type { Delivery } from "./delivery.ts";
import type {
  AddressId,
  Alias,
  DeliveryStage,
  Directory,
  Member,
  Message,
} from "./model.ts";
import {
  participant,
  type ReplyReference,
  type Room,
  recipients,
  type ThreadGroup,
} from "./store.ts";

export type RouteState = "checking" | "failed";
export type SendForm = "anyway" | "to";
export type RoutePart =
  | { kind: "state"; state: RouteState }
  | { kind: "names"; ids: readonly AddressId[] }
  | { kind: "reason"; text: string }
  | { kind: "pulled-in" }
  | { kind: "receipt"; id: AddressId; named: boolean; stage: DeliveryStage }
  | { kind: "mentioned"; aliases: readonly string[] }
  | { kind: "send"; aliases: readonly Alias[]; form: SendForm }
  | {
      kind: "delivery";
      alias: Alias;
      state: Exclude<Delivery, { kind: "idle" }>;
    }
  | { kind: "replies"; count: number };
export type RouteTip =
  | { kind: "router"; name: string }
  | { kind: "unreachable" };
export type RouteLine = {
  icon: "clock" | "arrow" | "inbox" | "alert" | null;
  tone: "muted" | "failed";
  tip: RouteTip | null;
  parts: readonly RoutePart[];
};
// Everything channel chat adds around a message's body.
export type Chat = {
  kind: Room["kind"];
  reference: ReplyReference | null;
  route: RouteLine | null;
};

// The operator writes as "human", whatever id a leg names them by.
function writer(directory: Directory, id: AddressId): AddressId | "human" {
  return participant(directory, id).kind === "operator" ? "human" : id;
}
// True when every one of `ids` wrote in the room at or after `at`, so a
// follow-up keeps an honest "Working on it" after an earlier answer.
export function answered(
  room: Room,
  ids: readonly AddressId[],
  at: number,
  directory: Directory,
): boolean {
  return (
    ids.length > 0 &&
    ids.every(
      (id) =>
        (room.wrote.get(writer(directory, id)) ?? Number.NEGATIVE_INFINITY) >=
        at,
    )
  );
}

function line(
  icon: RouteLine["icon"],
  parts: readonly RoutePart[],
  tip: RouteTip | null = null,
): RouteLine {
  return { icon, tone: "muted", tip, parts };
}
function repliesOnly(tail: readonly RoutePart[]): RouteLine | null {
  return tail.length === 0 ? null : line(null, tail);
}

function liveAgentNamed(
  directory: Directory,
  alias: string,
): Member | undefined {
  for (const member of directory.members.values())
    if (
      member.alias === alias &&
      member.kind.type === "agent" &&
      member.activity !== "retired"
    )
      return member;
  return undefined;
}

const unknown = { state: "unknown" } as const;
const unknownStage: DeliveryStage = { stage: "unknown" };

// The line under a channel-chat message: who got it and why, its receipts,
// the mentioned agents it did not reach, and the post's reply count.
export function routeLine(
  message: Message,
  room: Extract<Room, { kind: "channel" }>,
  directory: Directory,
  replies: number,
  delivery: (alias: Alias) => Delivery,
): RouteLine | null {
  const reached = recipients(message).filter(
    (id) => participant(directory, id).kind !== "operator",
  );
  const tail: readonly RoutePart[] =
    replies > 0 ? [{ kind: "replies", count: replies }] : [];
  const names: RoutePart = { kind: "names", ids: reached };
  const receipts = (): RoutePart[] =>
    answered(room, reached, message.at, directory)
      ? []
      : reached.map((id) => ({
          kind: "receipt",
          id,
          named: reached.length > 1,
          stage:
            message.legs.find((leg) => leg.to === id)?.stage ?? unknownStage,
        }));
  // An agent the thread already reached, by this post or a later message, or
  // who wrote in it, has the conversation, so it is not offered again.
  const offers = () => {
    const left = (message.routing?.mentioned ?? [])
      .filter((alias) => {
        const member = liveAgentNamed(directory, alias);
        return member !== undefined && !room.joined.has(member.id);
      })
      .map((alias) => ({ alias, state: delivery(alias) }));
    return {
      idle: left.flatMap(({ alias, state }) =>
        state.kind === "idle" ? [alias] : [],
      ),
      pressed: left.flatMap(({ alias, state }): RoutePart[] =>
        state.kind === "idle" ? [] : [{ kind: "delivery", alias, state }],
      ),
    };
  };
  const pulledIn = reached.some((id) => {
    const who = participant(directory, id);
    return (
      who.kind === "member" &&
      message.woke.some((alias) => alias === who.member.alias) &&
      room.joined.get(id) === message.at
    );
  });
  // A post the router answered: who it reached, then the mentions it did not.
  const sent = (reason: string | null, router: string | null) => {
    const { idle, pressed } = offers();
    const unsent: RoutePart[] = [
      ...(idle.length === 0
        ? []
        : [
            { kind: "mentioned", aliases: idle } as const,
            {
              kind: "send",
              aliases: idle,
              form: idle.length === 1 ? "anyway" : "to",
            } as const,
          ]),
      ...pressed,
    ];
    if (reached.length === 0)
      return unsent.length === 0
        ? repliesOnly(tail)
        : line("inbox", [...unsent, ...tail]);
    return line(
      "arrow",
      [
        names,
        ...(reason === null ? [] : [{ kind: "reason", text: reason } as const]),
        ...receipts(),
        ...unsent,
        ...tail,
      ],
      router === null ? null : { kind: "router", name: router },
    );
  };
  const routing = message.routing ?? unknown;
  switch (routing.state) {
    case "routing":
      return line("clock", [{ kind: "state", state: "checking" }, ...tail]);
    case "routed":
      return sent(routing.reason, routing.router);
    case "unrouted":
      return sent(null, null);
    case "router_failed": {
      const { idle, pressed } = offers();
      return {
        icon: "alert",
        tone: "failed",
        tip: { kind: "unreachable" },
        parts: [
          { kind: "state", state: "failed" },
          ...(idle.length === 0
            ? []
            : [{ kind: "send", aliases: idle, form: "to" } as const]),
          ...pressed,
          ...tail,
        ],
      };
    }
    case "unknown":
      return reached.length === 0
        ? repliesOnly(tail)
        : line("arrow", [
            names,
            ...(pulledIn ? [{ kind: "pulled-in" } as const] : []),
            ...receipts(),
            ...tail,
          ]);
    default: {
      const exhaustive: never = routing;
      return exhaustive;
    }
  }
}

export function chatOf({
  message,
  thread,
  reference,
  directory,
  replies,
  delivery,
}: {
  message: Message;
  thread: ThreadGroup;
  reference: ReplyReference | null;
  directory: Directory;
  replies: number;
  delivery: (alias: Alias) => Delivery;
}): Chat | null {
  const { room } = thread;
  if (room === null) return null;
  return {
    kind: room.kind,
    reference,
    route:
      room.kind === "channel"
        ? routeLine(message, room, directory, replies, delivery)
        : repliesOnly(replies > 0 ? [{ kind: "replies", count: replies }] : []),
  };
}

// The view model is plain data, so equal values render the same.
export function sameChat(a: Chat | null, b: Chat | null): boolean {
  if (a === null || b === null) return a === b;
  return (
    a.kind === b.kind &&
    (a.reference === b.reference ||
      (a.reference !== null &&
        b.reference !== null &&
        a.reference.kind === b.reference.kind &&
        (a.reference.kind === "earlier" ||
          (b.reference.kind === "message" &&
            a.reference.parent === b.reference.parent)))) &&
    JSON.stringify(a.route) === JSON.stringify(b.route)
  );
}

// The people a chat names through the directory, so a rename re-renders it.
export function chatPeople(
  chat: Chat | null,
): readonly (AddressId | "human")[] {
  if (chat === null) return [];
  const people: (AddressId | "human")[] = [];
  if (chat.reference?.kind === "message")
    people.push(chat.reference.parent.author);
  for (const part of chat.route?.parts ?? [])
    if (part.kind === "names") people.push(...part.ids);
    else if (part.kind === "receipt") people.push(part.id);
  return people;
}
