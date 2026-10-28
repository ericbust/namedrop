import type {
  Activity,
  AddressId,
  Attention,
  Channel,
  Health,
  Member,
  MemberKind,
  SafeText,
  SpaceKey,
  WireEvent,
} from "./model.ts";

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error("Expected object");
  return value as Record<string, unknown>;
}
function text(value: unknown): string {
  if (typeof value !== "string") throw new Error("Expected text");
  return value;
}
function identity(value: unknown): string {
  const result = text(value);
  if (!result.trim()) throw new Error("Expected identity");
  return result;
}
function spaceKey(value: unknown): SpaceKey {
  const key = text(value);
  if (!/^[0-9a-f]{20}$/.test(key)) throw new Error("Invalid space key");
  return key as SpaceKey;
}
function addressId(value: unknown): AddressId {
  return identity(value) as AddressId;
}
function nullableText(value: unknown): string | null {
  return value === null ? null : text(value);
}
function integer(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0)
    throw new Error("Expected nonnegative integer");
  return value;
}
function timestamp(value: unknown): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0)
    throw new Error("Expected nonnegative timestamp");
  return value;
}
function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error("Expected array");
  return value;
}
function safeText(value: unknown): SafeText {
  const item = record(value);
  if (typeof item.truncated !== "boolean")
    throw new Error("Expected truncation flag");
  return { text: text(item.text), truncated: item.truncated };
}
function optionalSafeText(value: unknown): SafeText | null {
  return value === null ? null : safeText(value);
}
function health(value: unknown): Health {
  const item = record(value);
  switch (item.level) {
    case "calm":
    case "paused":
    case "inactive":
      return { level: item.level };
    case "busy":
      return {
        level: item.level,
        inFlight: integer(item.in_flight),
        working: integer(item.working),
      };
    case "needs_you":
      return { level: item.level, items: integer(item.items) };
    default:
      throw new Error("Unknown health");
  }
}
function activity(value: unknown): Activity {
  switch (value) {
    case "retired":
    case "offline":
    case "blocked":
    case "working":
    case "waiting":
    case "idle":
      return value;
    default:
      throw new Error("Unknown activity");
  }
}
function memberKind(value: unknown): MemberKind {
  const item = record(value);
  switch (item.type) {
    case "human":
    case "terminal":
      return { type: item.type };
    case "agent":
      return { type: item.type, runtime: nullableText(item.runtime) };
    default:
      throw new Error("Unknown member kind");
  }
}
function attention(value: unknown): Attention {
  const item = record(value);
  const source = item.source_kind;
  switch (source) {
    case "delivery_attempt":
    case "automatic_reply_limit":
    case "pane_binding":
    case "integration_registration":
    case "agent_disposition":
    case "context_grant":
    case "coordination":
      return {
        source,
        summary: safeText(item.summary),
        openedAt: timestamp(item.opened_at_ms),
      };
    default:
      throw new Error("Unknown attention source");
  }
}
function channel(value: unknown, key: SpaceKey): Channel {
  const frame = record(value);
  if (frame.wire !== 1) throw new Error("Unknown wire version");
  const space = record(frame.space);
  if (spaceKey(space.key) !== key) throw new Error("Envelope key mismatch");
  const members = new Map<AddressId, Member>();
  for (const raw of array(frame.participants)) {
    const item = record(raw);
    const id = addressId(item.address_id);
    if (members.has(id)) throw new Error("Duplicate participant");
    members.set(id, {
      id,
      alias: text(item.alias),
      kind: memberKind(item.kind),
      activity: activity(item.activity),
      role: optionalSafeText(item.role),
      paneLabel: optionalSafeText(item.pane_label),
    });
  }
  const messages = new Map<string, { id: string }>();
  for (const raw of array(frame.messages)) {
    const id = identity(record(raw).message_id);
    if (messages.has(id)) throw new Error("Duplicate message");
    messages.set(id, { id });
  }
  return {
    key,
    workspaceId: identity(space.workspace_id),
    displayName: identity(space.display_name),
    addressableLabel: nullableText(space.addressable_label),
    health: health(frame.health),
    members,
    messages,
    attention: array(frame.attention).map(attention),
  };
}
export function parseEvent(name: string, rawJson: string): WireEvent | null {
  if (name !== "space" && name !== "gone" && name !== "ready") return null;
  try {
    const envelope = record(JSON.parse(rawJson) as unknown);
    if (name === "ready") {
      const values = array(envelope.keys).map(spaceKey);
      const keys = new Set(values);
      return { type: "ready", keys };
    }
    const key = spaceKey(envelope.key);
    return name === "gone"
      ? { type: "gone", key }
      : { type: "space", channel: channel(envelope.frame, key) };
  } catch {
    return null;
  }
}
