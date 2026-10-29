export type SpaceKey = string & { readonly __brand: "SpaceKey" };
export type AddressId = string & { readonly __brand: "AddressId" };

export type Activity =
  | "retired"
  | "offline"
  | "blocked"
  | "working"
  | "waiting"
  | "idle"
  | "unknown";
export type Health =
  | { level: "calm" | "paused" | "inactive" | "unknown" }
  | { level: "busy"; inFlight: number; working: number }
  | { level: "needs_you"; items: number };
export type MemberKind =
  | { type: "human" | "terminal" | "unknown" }
  | { type: "agent"; runtime: string | null };
export type SafeText = { text: string; truncated: boolean };
export type Member = {
  id: AddressId;
  alias: string;
  kind: MemberKind;
  activity: Activity;
  role: SafeText | null;
  paneLabel: SafeText | null;
};
export type Attention = {
  rowKey: string;
  source:
    | "delivery_attempt"
    | "automatic_reply_limit"
    | "pane_binding"
    | "integration_registration"
    | "agent_disposition"
    | "context_grant"
    | "coordination"
    | "agent_status"
    | "unknown";
  summary: SafeText;
  openedAt: number;
  subjectAlias: string | null;
  messageId: string | null;
};
export type TerminalRun = {
  terminal: AddressId;
  command: SafeText;
  startedAt: number;
  exitCode: number | null;
  messageId: string | null;
};
export type TerminalResult = {
  exitCode: number;
  stdout: string;
  stderr: string;
};
export type MessageKind =
  | "mention"
  | "reply"
  | "handoff"
  | "result_set"
  | "nudge"
  | "hook"
  | "unknown";
export type DeliveryStage =
  | {
      stage:
        | "queued"
        | "delivered"
        | "held"
        | "failed"
        | "cancelled"
        | "unknown";
    }
  | { stage: "working"; ackedAt: number }
  | { stage: "replied"; replyId: string }
  | { stage: "completed"; kind: string };
// A member alias as `Member.alias` holds it: one "@", then the name.
export type Alias = `@${string}`;
// Where a channel post stands with the router. A message carries one exactly
// when its wire row has a `channel` object, so a state a newer server invents
// still marks the post as channel chat. Who got it comes from the legs.
export type Routing =
  | { state: "routing" }
  | { state: "routed"; reason: string | null; router: string | null }
  | { state: "unrouted" }
  | { state: "router_failed" }
  | { state: "unknown" };
export type Message = {
  id: string;
  threadId: string;
  inReplyTo: string | null;
  at: number;
  author: AddressId | "human";
  kind: MessageKind;
  body: SafeText;
  legs: readonly { to: AddressId; stage: DeliveryStage }[];
  routing: Routing | null;
  woke: readonly Alias[];
};
export type ThreadChip = { id: string; title: SafeText; messageCount: number };
export type HistoryRequest = {
  id: number;
  key: SpaceKey;
  before: string;
  purpose: "older" | "bridge";
};
export type HistoryState =
  | { kind: "idle" }
  | { kind: "loading"; request: HistoryRequest }
  | { kind: "error"; request: HistoryRequest; reason: string };
export type SpaceState =
  | "active"
  | "paused"
  | "not_activated"
  | "deactivated"
  | "unknown";
export type Mention =
  | { kind: "operator" }
  | { kind: "alias"; alias: string }
  | { kind: "envoy"; id: AddressId };
export type MailState =
  | "queued"
  | "dispatching"
  | "submitted"
  | "acknowledged"
  | "held"
  | "unknown";
export type MailItem = Readonly<{
  from: Mention;
  preview: SafeText;
  state: MailState;
  since: number;
}>;
export type MemberWork = Readonly<{
  mailbox: readonly MailItem[];
  waitingOn: readonly Mention[];
}>;
export type ChannelSnapshot = {
  key: SpaceKey;
  workspaceId: string;
  displayName: string;
  addressableLabel: string | null;
  state: SpaceState;
  health: Health;
  members: ReadonlyMap<AddressId, Member>;
  work: ReadonlyMap<AddressId, MemberWork>;
  messages: ReadonlyMap<string, Message>;
  attention: readonly Attention[];
  threads: ReadonlyMap<string, ThreadChip>;
  terminalRuns: readonly TerminalRun[];
};
export type Channel = ChannelSnapshot & {
  latestMessages: ReadonlyMap<string, Message>;
  viewMode: "following" | "browsing";
  atBottom: boolean;
  runs: readonly (readonly string[])[];
  history: HistoryState;
  exhausted: boolean;
};
export type OutgoingPhase =
  | { kind: "sending" }
  | { kind: "sent"; messageId: string };
export type ComposerId = { key: SpaceKey; thread: string | null };
export type DraftKey = `channel:${SpaceKey}` | `thread:${string}`;
export type SendAttempt = {
  requestId: string;
  source: string;
  inReplyTo: string | null;
};
export type SendFailure = { status: number; message: string };
// A refused draft keeps its attempt until a success, so the same post reuses
// its id. The failure shows only until the next edit, and never without an attempt.
export type Draft = ComposerId & { text: string } & (
    | { attempt: null; failure: null }
    | { attempt: SendAttempt; failure: SendFailure | null }
  );
export type Outgoing = {
  requestId: string;
  composer: ComposerId;
  source: string;
  inReplyTo: string | null;
  at: number;
  phase: OutgoingPhase;
};
export type Home = {
  key: SpaceKey;
  name: string;
  token: string | null;
  members: ReadonlyMap<string, Member>;
};
export type Directory = {
  members: ReadonlyMap<AddressId, Member>;
  homes: ReadonlyMap<string, Home>;
};
export type Participant =
  | { kind: "operator" }
  | { kind: "member"; member: Member; home: Home | null }
  | { kind: "envoy"; id: AddressId; home: Home | null }
  | { kind: "unknown"; id: AddressId };
export type ReplyTo = { names: readonly string[]; mentions: string };
export type WireEvent =
  | { type: "space"; channel: ChannelSnapshot }
  | { type: "gone"; key: SpaceKey }
  | { type: "ready"; keys: ReadonlySet<SpaceKey> };
export type Event =
  | WireEvent
  | { type: "older"; key: SpaceKey }
  | { type: "latest"; key: SpaceKey }
  | { type: "follow"; key: SpaceKey; atBottom: boolean }
  | {
      type: "history";
      request: HistoryRequest;
      result:
        | { kind: "page"; messages: readonly Message[] }
        | { kind: "error"; reason: string };
    }
  | { type: "select"; key: SpaceKey }
  | { type: "navigate"; key: SpaceKey | null }
  | { type: "read"; key: SpaceKey }
  | { type: "thread"; threadId: string | null }
  | { type: "activity" }
  | { type: "jump"; key: SpaceKey }
  | { type: "peek"; target: PeekTarget | null }
  | { type: "opened" }
  | { type: "replay-timeout" }
  | { type: "discovery-timeout" }
  | { type: "input" }
  | { type: "disconnected" }
  | { type: "visibility"; visible: boolean }
  | { type: "edit"; composer: ComposerId; text: string }
  | { type: "fill"; composer: ComposerId; text: string }
  | { type: "send"; outgoing: Omit<Outgoing, "phase"> }
  | { type: "sent"; requestId: string; messageId: string }
  | { type: "send-failed"; requestId: string; status: number; message: string };
export type Sync =
  | { kind: "connecting" | "reconnecting" | "live" | "fallback" }
  | {
      kind: "replaying";
      status: "connecting" | "reconnecting";
      pending: Map<SpaceKey, Channel>;
    };
export type Selection =
  | { kind: "none"; key: null }
  | { kind: "automatic"; key: SpaceKey }
  | { kind: "chosen"; key: SpaceKey }
  | { kind: "activity"; key: null };
export type Landing =
  | { kind: "discovering" }
  | { kind: "awaiting-url" }
  | { kind: "settled" };
export type State = {
  channels: Map<SpaceKey, Channel>;
  selection: Selection;
  landing: Landing;
  requestedKey: SpaceKey | null;
  slot: Slot;
  unread: ReadonlySet<SpaceKey>;
  sync: Sync;
  visible: boolean;
  viewKnown: boolean;
  nextRequestId: number;
  outbox: ReadonlyMap<string, Outgoing>;
  drafts: ReadonlyMap<DraftKey, Draft>;
};
export type PeekableKind = Exclude<MemberKind["type"], "human">;
export type PeekTarget = Readonly<{
  channel: SpaceKey;
  member: AddressId;
  alias: string;
  kind: PeekableKind;
}> & { readonly __brand: "PeekTarget" };
export type Slot =
  | { kind: "closed" }
  | { kind: "thread"; id: string }
  | { kind: "peek"; target: PeekTarget };
export type Route =
  | { kind: "root" }
  | { kind: "channel"; key: SpaceKey }
  | { kind: "activity" };
export type Screen = Readonly<{ earlierOmitted: boolean; text: string }>;
export type ScreenFailure =
  | "closed"
  | "missing"
  | "busy"
  | "session"
  | "unreachable";
export type ScreenResult =
  | { kind: "text"; screen: Screen }
  | { kind: "failed"; failure: ScreenFailure };
