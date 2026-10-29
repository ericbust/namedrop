import type {
  Activity,
  AddressId,
  Attention,
  ChannelSnapshot,
  DeliveryStage,
  Health,
  Member,
  MemberKind,
  Message,
  MessageKind,
  SafeText,
  SpaceKey,
  TerminalResult,
  TerminalRun,
  ThreadChip,
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
function optionalIdentity(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}
function optionalAlias(value: unknown): string | null {
  const bare = typeof value === "string" ? value.replace(/^@+/, "") : "";
  return bare.trim() ? `@${bare}` : null;
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
  const level = identity(item.level);
  switch (level) {
    case "calm":
    case "paused":
    case "inactive":
      return { level };
    case "busy":
      return {
        level,
        inFlight: integer(item.in_flight),
        working: integer(item.working),
      };
    case "needs_you":
      return { level, items: integer(item.items) };
    default:
      return { level: "unknown" };
  }
}
function activity(value: unknown): Activity {
  const result = identity(value);
  switch (result) {
    case "retired":
    case "offline":
    case "blocked":
    case "working":
    case "waiting":
    case "idle":
      return result;
    default:
      return "unknown";
  }
}
function memberKind(value: unknown): MemberKind {
  const item = record(value);
  const type = identity(item.type);
  switch (type) {
    case "human":
    case "terminal":
      return { type };
    case "agent":
      return { type, runtime: nullableText(item.runtime) };
    default:
      return { type: "unknown" };
  }
}
function attention(
  value: unknown,
  occurrences: Map<string, number>,
): Attention {
  const item = record(value);
  const kind = identity(item.source_kind);
  let source: Attention["source"];
  switch (kind) {
    case "delivery_attempt":
    case "automatic_reply_limit":
    case "pane_binding":
    case "integration_registration":
    case "agent_disposition":
    case "context_grant":
    case "coordination":
    case "agent_status":
      source = kind;
      break;
    default:
      source = "unknown";
  }
  const summary = safeText(item.summary);
  const openedAt = timestamp(item.opened_at_ms);
  const fingerprint = JSON.stringify([
    kind,
    summary.text,
    summary.truncated,
    openedAt,
  ]);
  const occurrence = occurrences.get(fingerprint) ?? 0;
  occurrences.set(fingerprint, occurrence + 1);
  return {
    rowKey: JSON.stringify([fingerprint, occurrence]),
    source,
    summary,
    openedAt,
    subjectAlias: optionalAlias(item.subject_alias),
    messageId: optionalIdentity(item.message_id),
  };
}
function terminalRuns(value: unknown): TerminalRun[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((raw): TerminalRun[] => {
    try {
      const item = record(raw);
      const exitCode = item.exit_code;
      return [
        {
          terminal: addressId(item.terminal),
          command: safeText(item.command),
          startedAt: timestamp(item.started_at_ms),
          exitCode:
            typeof exitCode === "number" && Number.isSafeInteger(exitCode)
              ? exitCode
              : null,
          messageId: optionalIdentity(item.message_id),
        },
      ];
    } catch {
      return [];
    }
  });
}
function messageKind(value: unknown): MessageKind {
  switch (identity(value)) {
    case "mention":
      return "mention";
    case "reply":
      return "reply";
    case "handoff":
      return "handoff";
    case "result_set":
      return "result_set";
    case "nudge":
      return "nudge";
    case "hook":
      return "hook";
    default:
      return "unknown";
  }
}
function deliveryStage(value: unknown): DeliveryStage {
  const item = record(value);
  const stage = identity(item.stage);
  switch (stage) {
    case "queued":
    case "delivered":
    case "held":
    case "failed":
    case "cancelled":
      return { stage };
    case "working":
      return { stage, ackedAt: timestamp(item.acked_at_ms) };
    case "replied":
      return { stage, replyId: identity(item.reply_id) };
    case "completed":
      return { stage, kind: identity(item.kind) };
    default:
      return { stage: "unknown" };
  }
}
function message(value: unknown): Message {
  const item = record(value);
  return {
    id: identity(item.message_id),
    threadId: identity(item.thread_id),
    inReplyTo: item.in_reply_to === null ? null : identity(item.in_reply_to),
    at: timestamp(item.at_ms),
    author: item.author === "human" ? "human" : addressId(item.author),
    kind: messageKind(item.kind),
    body: safeText(item.body),
    legs: array(item.legs).map((raw) => {
      const leg = record(raw);
      return { to: addressId(leg.to), stage: deliveryStage(leg.stage) };
    }),
  };
}
function messages(value: unknown): Map<string, Message> {
  const rows = new Map<string, Message>();
  for (const raw of array(value)) {
    const row = message(raw);
    if (rows.has(row.id)) throw new Error("Duplicate message");
    rows.set(row.id, row);
  }
  return rows;
}
const TERMINAL_HEADER =
  /^@\S+ command finished with exit code (-?\d+)\.\nstdout:\n/;
const STDERR = "\nstderr:\n";
const EMPTY_STDERR = "\nstderr:";
export function terminalResult({
  text,
  truncated,
}: SafeText): TerminalResult | null {
  const header = TERMINAL_HEADER.exec(text);
  if (header === null) return null;
  const exitCode = Number(header[1]);
  if (!Number.isSafeInteger(exitCode)) return null;
  const output = text.slice(header[0].length);
  let split = output.lastIndexOf(STDERR);
  let stderr = "";
  if (output.endsWith(EMPTY_STDERR))
    split = output.length - EMPTY_STDERR.length;
  else if (split >= 0) stderr = output.slice(split + STDERR.length);
  else if (truncated) split = output.length;
  else return null;
  const stdout = output.slice(0, split);
  return {
    exitCode,
    stdout: stdout.endsWith("\n") ? stdout.slice(0, -1) : stdout,
    stderr,
  };
}
export function pathKey(pathname: string): SpaceKey | null {
  const segment = /^\/w\/([0-9a-f]{20})\/?$/.exec(pathname)?.[1];
  return segment === undefined ? null : spaceKey(segment);
}
export function parseHistory(raw: unknown): readonly Message[] {
  return [...messages(raw).values()].reverse();
}
function channel(value: unknown, key: SpaceKey): ChannelSnapshot {
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
  const rows = messages(frame.messages);
  const threads = new Map<string, ThreadChip>();
  for (const raw of array(frame.threads)) {
    const item = record(raw);
    const id = identity(item.thread_id);
    threads.set(id, {
      id,
      title: safeText(item.title),
      messageCount: integer(item.message_count),
    });
  }
  const attentionOccurrences = new Map<string, number>();
  return {
    key,
    workspaceId: identity(space.workspace_id),
    displayName: identity(space.display_name),
    addressableLabel: nullableText(space.addressable_label),
    health: health(frame.health),
    members,
    messages: rows,
    threads,
    attention: array(frame.attention).map((item) =>
      attention(item, attentionOccurrences),
    ),
    terminalRuns: terminalRuns(frame.terminal_runs),
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
