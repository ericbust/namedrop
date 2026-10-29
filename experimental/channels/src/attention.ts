import type {
  Attention,
  AttentionResponse,
  ChannelSnapshot,
  Conversation,
  Directory,
  Message,
  MessageContext,
  ReplyTo,
  SpaceKey,
} from "./model.ts";
import { participant, type ThreadGroup } from "./store.ts";

export type ReplyIntent = Extract<AttentionResponse, { kind: "reply" }>;
export type MessageIntent = Extract<AttentionResponse, { kind: "message" }> & {
  reference: { kind: "coordination" | "message"; id: string };
  preparation: "idle" | "requested" | "prepared" | "preserved" | "unavailable";
};
export type EvidenceResponse =
  | ReplyIntent
  | MessageIntent
  | Extract<AttentionResponse, { kind: "pane" }>;
export type EvidenceRequest = {
  key: SpaceKey;
  target: Conversation;
  response: EvidenceResponse | null;
};
export type FocusedEvidence =
  | { kind: "closed"; version: number }
  | { kind: "loading"; version: number; request: EvidenceRequest }
  | { kind: "error"; version: number; request: EvidenceRequest; reason: string }
  | {
      kind: "ready";
      version: number;
      request: EvidenceRequest;
      context: MessageContext;
    };
export type EvidenceEvent =
  | { type: "open"; request: EvidenceRequest }
  | { type: "retry" }
  | { type: "prepare-message" }
  | {
      type: "message-prepared";
      version: number;
      result: "prepared" | "preserved" | "unavailable";
    }
  | { type: "close" }
  | { type: "sent"; key: SpaceKey; messageId: string | null }
  | { type: "loaded"; version: number; context: MessageContext }
  | { type: "failed"; version: number; reason: string };
export function evidenceReduce(
  state: FocusedEvidence,
  event: EvidenceEvent,
): FocusedEvidence {
  switch (event.type) {
    case "open":
      return {
        kind: "loading",
        version: state.version + 1,
        request: event.request,
      };
    case "retry":
      return state.kind === "error"
        ? {
            kind: "loading",
            version: state.version + 1,
            request: state.request,
          }
        : state;
    case "close":
      return { kind: "closed", version: state.version + 1 };
    case "prepare-message":
      return state.kind === "ready" &&
        state.request.response?.kind === "message"
        ? {
            ...state,
            request: {
              ...state.request,
              response: { ...state.request.response, preparation: "requested" },
            },
          }
        : state;
    case "message-prepared":
      return state.kind === "ready" &&
        state.version === event.version &&
        state.request.response?.kind === "message" &&
        state.request.response.preparation === "requested"
        ? {
            ...state,
            request: {
              ...state.request,
              response: {
                ...state.request.response,
                preparation: event.result,
              },
            },
          }
        : state;
    case "sent": {
      if (state.kind !== "ready" || state.request.key !== event.key)
        return state;
      const response = state.request.response;
      if (response?.kind === "reply" && response.messageId === event.messageId)
        return { ...state, request: { ...state.request, response: null } };
      if (response?.kind === "message" && event.messageId === null)
        return {
          ...state,
          request: {
            ...state.request,
            response: { ...response, preparation: "idle" },
          },
        };
      return state;
    }
    case "loaded":
      return state.kind === "loading" && state.version === event.version
        ? { ...state, kind: "ready", context: event.context }
        : state;
    case "failed":
      return state.kind === "loading" && state.version === event.version
        ? { ...state, kind: "error", reason: event.reason }
        : state;
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}
export function attentionConversation(
  item: Attention,
  channel: Pick<ChannelSnapshot, "messages">,
): Conversation | null {
  if (item.conversation) return item.conversation;
  const legacy =
    item.messageId === null ? undefined : channel.messages.get(item.messageId);
  return legacy === undefined
    ? null
    : { threadId: legacy.threadId, messageId: legacy.id };
}
export function evidenceThread(
  context: MessageContext,
  live: ReadonlyMap<string, Message>,
): ThreadGroup {
  const rows = new Map(context.messages.map((row) => [row.id, row]));
  for (const row of live.values())
    if (row.threadId === context.threadId) rows.set(row.id, row);
  const messages = [...rows.values()].sort(
    (a, b) => a.at - b.at || a.id.localeCompare(b.id),
  );
  return {
    id: context.threadId,
    parents: messages.filter(
      (row) => row.inReplyTo === null && row.kind !== "reply",
    ),
    replies: messages.filter(
      (row) => row.inReplyTo !== null || row.kind === "reply",
    ),
    knownCount: Math.max(context.messageCount, messages.length),
  };
}
export function attentionReplyTo(
  intent: Pick<ReplyIntent, "addressId">,
  directory: Directory,
): ReplyTo | null {
  const who = participant(directory, intent.addressId);
  if (
    who.kind !== "member" ||
    who.member.kind.type !== "agent" ||
    who.member.activity === "retired"
  )
    return null;
  const alias = who.member.alias;
  if (!/^@[a-z0-9]+(?:-[a-z0-9]+)*$/.test(alias)) return null;
  if (who.home === null) return { names: [alias], mentions: alias };
  return who.home.token === null
    ? null
    : {
        names: [`${alias} · ${who.home.token}`],
        mentions: `${who.home.token} ${alias}`,
      };
}
export function replyPrefill(
  draft: string,
  to: ReplyTo,
): { text: string; preserved: boolean } {
  return draft.length === 0
    ? { text: `${to.mentions} `, preserved: false }
    : { text: draft, preserved: true };
}

export function messageIntent(
  item: Attention,
  target: Conversation,
  prepare: boolean,
): MessageIntent | null {
  return item.response?.kind === "message"
    ? {
        ...item.response,
        reference: item.coordinationId
          ? { kind: "coordination", id: item.coordinationId }
          : { kind: "message", id: target.messageId },
        preparation: prepare ? "requested" : "idle",
      }
    : null;
}
export function prepareChannelMessage(
  intent: MessageIntent,
  directory: Directory,
  draft: string,
  pending: boolean,
): { kind: "prepared"; text: string } | { kind: "preserved" | "unavailable" } {
  if (draft.length > 0 || pending) return { kind: "preserved" };
  const to = attentionReplyTo(intent, directory);
  return to === null
    ? { kind: "unavailable" }
    : {
        kind: "prepared",
        text: `${to.mentions} Regarding ${intent.reference.kind} ${intent.reference.id}: `,
      };
}
