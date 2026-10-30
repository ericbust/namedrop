import type {
  AddressId,
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
// "anyway" offers the one mentioned agent the post did not reach; "to" lists each.
export type SendForm = "anyway" | "to";
export type RoutePart =
  | { kind: "state"; state: RouteState }
  | { kind: "names"; ids: readonly AddressId[] }
  | { kind: "reason"; text: string }
  | { kind: "pulled-in" }
  | { kind: "receipt"; id: AddressId; named: boolean; stage: DeliveryStage }
  | { kind: "mentioned"; aliases: readonly string[] }
  | { kind: "send"; aliases: readonly string[]; form: SendForm }
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

// The current agent with this alias. A retired pane's alias can be reused.
function agentNamed(directory: Directory, alias: string): Member | undefined {
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
  const offers = () =>
    (message.routing?.mentioned ?? []).filter((alias) => {
      const member = agentNamed(directory, alias);
      return member !== undefined && !room.joined.has(member.id);
    });
  // A woken member is pulled in only when this message brings it into the
  // conversation, so the asker hearing back reads as a plain recipient.
  const pulledIn = message.woke.some((alias) => {
    const member = agentNamed(directory, alias);
    return (
      member === undefined ||
      (room.joined.get(member.id) ?? message.at) >= message.at
    );
  });
  // A post the router answered: who it reached, then the mentions it did not.
  const sent = (reason: string | null, router: string | null) => {
    const left = offers();
    const unsent: RoutePart[] =
      left.length === 0
        ? []
        : [
            { kind: "mentioned", aliases: left },
            {
              kind: "send",
              aliases: left,
              form: left.length === 1 ? "anyway" : "to",
            },
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
      const left = offers();
      return {
        icon: "alert",
        tone: "failed",
        tip: { kind: "unreachable" },
        parts: [
          { kind: "state", state: "failed" },
          ...(left.length === 0
            ? []
            : [{ kind: "send", aliases: left, form: "to" } as const]),
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
}: {
  message: Message;
  thread: ThreadGroup;
  reference: ReplyReference | null;
  directory: Directory;
  replies: number;
}): Chat | null {
  const { room } = thread;
  if (room === null) return null;
  return {
    kind: room.kind,
    reference,
    route:
      room.kind === "channel"
        ? routeLine(message, room, directory, replies)
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
