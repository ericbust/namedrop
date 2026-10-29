import type {
  Activity,
  AddressId,
  Attention,
  ChannelSnapshot,
  DeliveryStage,
  Health,
  MailItem,
  MailState,
  Member,
  MemberKind,
  MemberWork,
  Mention,
  Message,
  MessageKind,
  Route,
  SafeText,
  Screen,
  ScreenFailure,
  SpaceKey,
  SpaceState,
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
    subjectSummary: optionalSafeText(item.subject_summary ?? null),
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
function spaceState(value: unknown): SpaceState {
  switch (value) {
    case "active":
    case "paused":
    case "not_activated":
    case "deactivated":
      return value;
    default:
      return "unknown";
  }
}
function mailState(value: unknown): MailState {
  switch (value) {
    case "queued":
    case "dispatching":
    case "submitted":
    case "acknowledged":
    case "held":
      return value;
    default:
      return "unknown";
  }
}
export const ENVOY = /^envoy:([^:]+):(.+)$/;
function mention(value: unknown): Mention | null {
  if (value === "you") return { kind: "operator" };
  const alias = optionalAlias(value);
  if (alias === null) return null;
  const bare = alias.slice(1);
  return ENVOY.test(bare)
    ? { kind: "envoy", id: addressId(bare) }
    : { kind: "alias", alias };
}
function mailItem(value: unknown): MailItem[] {
  try {
    const item = record(value);
    const from = mention(item.from);
    if (from === null) return [];
    return [
      {
        from,
        preview: safeText(item.preview),
        state: mailState(item.state),
        since: timestamp(item.since_ms),
      },
    ];
  } catch {
    return [];
  }
}
function memberWork(item: Record<string, unknown>): MemberWork | null {
  const mailbox = Array.isArray(item.mailbox)
    ? item.mailbox.flatMap(mailItem)
    : [];
  const waitingOn = Array.isArray(item.waiting_on)
    ? item.waiting_on.flatMap((raw) => mention(raw) ?? [])
    : [];
  return mailbox.length === 0 && waitingOn.length === 0
    ? null
    : { mailbox, waitingOn };
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
export function routeOf({
  pathname,
  search,
}: {
  pathname: string;
  search: string;
}): Route {
  const segment = /^\/w\/([0-9a-f]{20})\/?$/.exec(pathname)?.[1];
  if (segment !== undefined) return { kind: "channel", key: spaceKey(segment) };
  return pathname === "/" &&
    new URLSearchParams(search).get("view") === "activity"
    ? { kind: "activity" }
    : { kind: "root" };
}
export function routePath(route: Route): string {
  switch (route.kind) {
    case "root":
      return "/";
    case "channel":
      return `/w/${route.key}`;
    case "activity":
      return "/?view=activity";
    default: {
      const exhaustive: never = route;
      return exhaustive;
    }
  }
}
export function sameRoute(a: Route, b: Route): boolean {
  return routePath(a) === routePath(b);
}
const OMITTED = "[Earlier screen text omitted]\n";
export function parseScreen(body: string): Screen {
  return body.startsWith(OMITTED)
    ? { earlierOmitted: true, text: body.slice(OMITTED.length) }
    : { earlierOmitted: false, text: body };
}
export function screenFailure(status: number): ScreenFailure {
  switch (status) {
    case 410:
      return "closed";
    case 404:
      return "missing";
    case 503:
      return "busy";
    case 401:
    case 403:
      return "session";
    default:
      return "unreachable";
  }
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
  const work = new Map<AddressId, MemberWork>();
  for (const raw of array(frame.participants)) {
    const item = record(raw);
    const id = addressId(item.address_id);
    if (members.has(id)) throw new Error("Duplicate participant");
    const busy = memberWork(item);
    if (busy !== null) work.set(id, busy);
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
    state: spaceState(space.state),
    health: health(frame.health),
    members,
    work,
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
export function parseReceipt(raw: unknown): string {
  return identity(record(raw).message_id);
}
export function parseSessionToken(raw: unknown): string {
  return identity(record(raw).csrf_token);
}
export function parseRefusal(body: string): string {
  try {
    const error = record(JSON.parse(body) as unknown).error;
    if (typeof error === "string") return error.trim();
  } catch {}
  return body.trim();
}
