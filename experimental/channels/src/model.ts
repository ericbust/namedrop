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
export type Message = {
  id: string;
  threadId: string;
  inReplyTo: string | null;
  at: number;
  author: AddressId | "human";
  kind: MessageKind;
  body: SafeText;
  legs: readonly { to: AddressId; stage: DeliveryStage }[];
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
export type ChannelSnapshot = {
  key: SpaceKey;
  workspaceId: string;
  displayName: string;
  addressableLabel: string | null;
  health: Health;
  members: ReadonlyMap<AddressId, Member>;
  messages: ReadonlyMap<string, Message>;
  attention: readonly Attention[];
  threads: ReadonlyMap<string, ThreadChip>;
  terminalRuns: readonly TerminalRun[];
};
export type Channel = ChannelSnapshot & {
  latestMessages: ReadonlyMap<string, Message>;
  viewMode: "following" | "browsing";
  runs: readonly (readonly string[])[];
  history: HistoryState;
  exhausted: boolean;
};
export type WireEvent =
  | { type: "space"; channel: ChannelSnapshot }
  | { type: "gone"; key: SpaceKey }
  | { type: "ready"; keys: ReadonlySet<SpaceKey> };
export type Event =
  | WireEvent
  | { type: "older"; key: SpaceKey }
  | { type: "latest"; key: SpaceKey }
  | {
      type: "history";
      request: HistoryRequest;
      result:
        | { kind: "page"; messages: readonly Message[] }
        | { kind: "error"; reason: string };
    }
  | { type: "select"; key: SpaceKey }
  | { type: "read"; key: SpaceKey }
  | { type: "thread"; threadId: string | null }
  | { type: "opened" }
  | { type: "replay-timeout" }
  | { type: "disconnected" }
  | { type: "visibility"; visible: boolean };
export type Sync =
  | { kind: "connecting" | "reconnecting" | "live" }
  | { kind: "fallback"; preserveSelection: boolean }
  | {
      kind: "replaying";
      status: "connecting" | "reconnecting";
      pending: Map<SpaceKey, Channel>;
    };
export type State = {
  channels: Map<SpaceKey, Channel>;
  selectedKey: SpaceKey | null;
  requestedKey: SpaceKey | null;
  openThread: string | null;
  unread: ReadonlySet<SpaceKey>;
  sync: Sync;
  visible: boolean;
  viewKnown: boolean;
  nextRequestId: number;
};
