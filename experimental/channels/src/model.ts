export type SpaceKey = string & { readonly __brand: "SpaceKey" };
export type AddressId = string & { readonly __brand: "AddressId" };

export type Activity =
  | "retired"
  | "offline"
  | "blocked"
  | "working"
  | "waiting"
  | "idle";
export type Health =
  | { level: "calm" | "paused" | "inactive" }
  | { level: "busy"; inFlight: number; working: number }
  | { level: "needs_you"; items: number };
export type MemberKind =
  | { type: "human" | "terminal" }
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
  source:
    | "delivery_attempt"
    | "automatic_reply_limit"
    | "pane_binding"
    | "integration_registration"
    | "agent_disposition"
    | "context_grant"
    | "coordination";
  summary: SafeText;
  openedAt: number;
};
export type Message = { id: string };
export type Channel = {
  key: SpaceKey;
  workspaceId: string;
  displayName: string;
  addressableLabel: string | null;
  health: Health;
  members: ReadonlyMap<AddressId, Member>;
  messages: ReadonlyMap<string, Message>;
  attention: readonly Attention[];
};
export type WireEvent =
  | { type: "space"; channel: Channel }
  | { type: "gone"; key: SpaceKey }
  | { type: "ready"; keys: ReadonlySet<SpaceKey> };
export type Event =
  | WireEvent
  | { type: "select"; key: SpaceKey }
  | { type: "opened" }
  | { type: "disconnected" }
  | { type: "visibility"; visible: boolean };
export type Sync =
  | { kind: "connecting" | "reconnecting" | "live" }
  | {
      kind: "replaying";
      status: "connecting" | "reconnecting";
      pending: Map<SpaceKey, Channel>;
    };
export type State = {
  channels: Map<SpaceKey, Channel>;
  selectedKey: SpaceKey | null;
  unread: ReadonlySet<SpaceKey>;
  sync: Sync;
  visible: boolean;
};
