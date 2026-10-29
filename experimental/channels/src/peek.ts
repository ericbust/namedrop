import { peekCopy, peekFailureCopy } from "./copy.ts";
import type {
  Member,
  Participant,
  PeekTarget,
  Screen,
  ScreenFailure,
  ScreenResult,
  SpaceKey,
} from "./model.ts";
import type { PeekWho } from "./store.ts";

export type PeekKey = string & { readonly __brand: "PeekKey" };
export function peekKey(
  target: Pick<PeekTarget, "channel" | "member">,
): PeekKey {
  return `${target.channel}/${target.member}` as PeekKey;
}
export function samePeek(a: PeekTarget, b: PeekTarget): boolean {
  return a.channel === b.channel && a.member === b.member;
}
export function peekTarget(
  channel: SpaceKey,
  member: Member,
): PeekTarget | null {
  const { type } = member.kind;
  if (type === "human") return null;
  return {
    channel,
    member: member.id,
    alias: member.alias,
    kind: type,
  } as PeekTarget;
}
export function peekTargetOf(
  who: Participant,
  scope: SpaceKey | null,
): PeekTarget | null {
  if (who.kind !== "member") return null;
  const channel = who.home === null ? scope : who.home.key;
  return channel === null ? null : peekTarget(channel, who.member);
}

export const SLOW_AFTER_MS = 400;
export const BUSY_AFTER_MS = 2000;
export type Wait = "brief" | "slow" | "busy";
type Shown = Readonly<{ screen: Screen; at: number }>;
export type PeekRead =
  | { phase: "reading"; request: number; wait: Wait; shown: Shown | null }
  | { phase: "shown"; request: number; shown: Shown }
  | {
      phase: "failed";
      request: number;
      failure: ScreenFailure;
      shown: Shown | null;
    };
export type PeekReadEvent =
  | { type: "again" }
  | { type: "waited"; request: number; wait: Exclude<Wait, "brief"> }
  | { type: "settled"; request: number; result: ScreenResult; at: number };

export function firstRead(): PeekRead {
  return { phase: "reading", request: 1, wait: "brief", shown: null };
}
const waitRank = { brief: 0, slow: 1, busy: 2 } satisfies Record<Wait, number>;
export function peekReduce(read: PeekRead, event: PeekReadEvent): PeekRead {
  switch (event.type) {
    case "again":
      return read.phase === "reading"
        ? read
        : {
            phase: "reading",
            request: read.request + 1,
            wait: "brief",
            shown: read.shown,
          };
    case "waited":
      return read.phase === "reading" &&
        read.request === event.request &&
        waitRank[event.wait] > waitRank[read.wait]
        ? { ...read, wait: event.wait }
        : read;
    case "settled": {
      if (read.phase !== "reading" || read.request !== event.request)
        return read;
      const { result } = event;
      return result.kind === "text"
        ? {
            phase: "shown",
            request: read.request,
            shown: { screen: result.screen, at: event.at },
          }
        : {
            phase: "failed",
            request: read.request,
            failure: result.failure,
            shown: read.shown,
          };
    }
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}

export type PeekNotice = Readonly<{
  title: string;
  body: string;
  retry: boolean;
}>;
export type PeekBody =
  | { kind: "blank" }
  | { kind: "reading"; busy: boolean }
  | { kind: "empty" }
  | { kind: "text"; earlierOmitted: boolean; text: string }
  | { kind: "failure"; notice: PeekNotice };
export type PeekView = Readonly<{
  stamp: string | null;
  refreshing: boolean;
  bar: PeekNotice | null;
  body: PeekBody;
}>;
function screenBody(screen: Screen): PeekBody {
  return screen.text.trim() === ""
    ? { kind: "empty" }
    : {
        kind: "text",
        earlierOmitted: screen.earlierOmitted,
        text: screen.text,
      };
}
export function peekView(read: PeekRead, who: PeekWho): PeekView {
  const { shown } = read;
  const readAt = shown === null ? null : peekCopy.readAt(shown.at);
  switch (read.phase) {
    case "reading":
      return {
        stamp:
          read.wait === "brief" || shown === null ? readAt : peekCopy.reading,
        refreshing: true,
        bar: null,
        body:
          shown !== null
            ? screenBody(shown.screen)
            : read.wait === "brief"
              ? { kind: "blank" }
              : { kind: "reading", busy: read.wait === "busy" },
      };
    case "shown":
      return {
        stamp: readAt,
        refreshing: false,
        bar: null,
        body: screenBody(read.shown.screen),
      };
    case "failed": {
      const notice = peekFailureCopy[read.failure](who);
      return shown === null
        ? {
            stamp: null,
            refreshing: false,
            bar: null,
            body: { kind: "failure", notice },
          }
        : {
            stamp: readAt,
            refreshing: false,
            bar: notice,
            body: screenBody(shown.screen),
          };
    }
    default: {
      const exhaustive: never = read;
      return exhaustive;
    }
  }
}
