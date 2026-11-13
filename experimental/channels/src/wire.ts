import type { DirectHistory, DirectPeer } from "./direct-messages.ts";
import { type AttachedImage, isImageName, splitImages } from "./images.ts";
import type {
  Activity,
  AddressId,
  Alias,
  AnswerLost,
  Attention,
  AttentionResponse,
  ChannelSnapshot,
  Conversation,
  DeliverAnswer,
  DeliverResult,
  DeliveryStage,
  HandoffWarning,
  Health,
  MailItem,
  MailState,
  Member,
  MemberKind,
  MemberWork,
  Mention,
  Message,
  MessageContext,
  MessageKind,
  NotAddressableReason,
  PendingReminder,
  Repo,
  ReviewClear,
  Route,
  Routing,
  SafeText,
  Screen,
  ScreenFailure,
  SpaceKey,
  SpaceState,
  Subagent,
  TerminalResult,
  TerminalRun,
  ThreadAction,
  ThreadChip,
  WarningCause,
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
function optionalAlias(value: unknown): Alias | null {
  const bare = typeof value === "string" ? value.replace(/^@+/, "") : "";
  return bare.trim() ? `@${bare}` : null;
}
function nullableText(value: unknown): string | null {
  return value === null ? null : text(value);
}
// A server older than repo grouping omits the field.
function repo(value: unknown): Repo | null {
  if (value === undefined || value === null) return null;
  const item = record(value);
  return { id: identity(item.id), name: identity(item.name) };
}
function notAddressableReason(value: unknown): NotAddressableReason | null {
  switch (value) {
    case null:
      return null;
    case "missing_label":
    case "empty_label":
    case "reserved_all":
    case "shared_label":
    case "id_collision":
    case "not_activated":
    case "not_active":
    case "unavailable":
      return value;
    default:
      return "unavailable";
  }
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
function subagentText(value: unknown): SafeText | null {
  if (value === null || value === undefined) return null;
  try {
    const parsed = safeText(value);
    return {
      text: parsed.text.slice(0, 240),
      truncated: parsed.truncated || parsed.text.length > 240,
    };
  } catch {
    return null;
  }
}
function subagents(value: unknown): readonly Subagent[] {
  if (!Array.isArray(value)) return [];
  const agents = new Map<string, Subagent>();
  for (const raw of value) {
    try {
      const item = record(raw);
      const id = identity(item.id);
      const startedAt = integer(item.started_at_ms);
      agents.set(id, {
        id,
        kind: subagentText(item.kind),
        description: subagentText(item.description),
        startedAt,
      });
    } catch {
      // Optional metadata must not discard the participant or channel.
    }
  }
  return [...agents.values()];
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
function conversation(value: unknown): Conversation | null {
  if (value === undefined || value === null) return null;
  const item = record(value);
  return {
    threadId: identity(item.thread_id),
    messageId: identity(item.message_id),
  };
}
function attentionResponse(
  value: unknown,
  anchor: Conversation | null,
): AttentionResponse | null {
  if (value === undefined || value === null) return null;
  const item = record(value);
  switch (item.kind) {
    case "reply": {
      const target = {
        threadId: identity(item.thread_id),
        messageId: identity(item.message_id),
      };
      if (
        anchor === null ||
        anchor.threadId !== target.threadId ||
        anchor.messageId !== target.messageId
      )
        throw new Error("Reply does not match attention conversation");
      return {
        kind: "reply",
        ...target,
        addressId: addressId(item.address_id),
      };
    }
    case "message":
      if (anchor === null)
        throw new Error("Message response needs attention evidence");
      return { kind: "message", addressId: addressId(item.address_id) };
    case "pane":
      return { kind: "pane", addressId: addressId(item.address_id) };
    case "local":
      return { kind: "local" };
    default:
      return null;
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
    case "terminal_relay":
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
  const anchor = conversation(item.conversation);
  return {
    rowKey:
      optionalIdentity(item.attention_id) ??
      JSON.stringify([fingerprint, occurrence]),
    attentionId: optionalIdentity(item.attention_id),
    sourceId: optionalIdentity(item.source_id),
    coordinationId: optionalIdentity(item.coordination_id),
    conversation: anchor,
    response: attentionResponse(item.response, anchor),
    source,
    summary,
    subjectSummary: optionalSafeText(item.subject_summary ?? null),
    openedAt,
    subjectAlias: optionalAlias(item.subject_alias),
    messageId: optionalIdentity(item.message_id),
    threadId: optionalIdentity(item.thread_id),
    actions: threadActions(item.actions),
    handoff:
      kind === "coordination" && item.coordination != null
        ? handoffWarning(item.coordination)
        : null,
    answerLost:
      kind === "agent_disposition" ? answerLost(item.answer_lost) : null,
  };
}
function answerLost(value: unknown): AnswerLost | null {
  switch (value) {
    case undefined:
    case null:
      return null;
    case "retired":
    case "replaced":
    case "ended":
      return value;
    default:
      return "unknown";
  }
}
const causes: readonly WarningCause[] = [
  "blocked",
  "cancelled",
  "stalled",
  "lead_retired",
  "report",
];
function handoffWarning(value: unknown): HandoffWarning {
  const item = record(value);
  const cause = causes.find((known) => known === item.cause) ?? "report";
  const review =
    item.review == null
      ? null
      : (() => {
          const raw = record(item.review);
          return {
            to: addressId(raw.to),
            requestedAt: timestamp(raw.requested_at_ms),
            expiresAt: timestamp(raw.expires_at_ms),
          };
        })();
  return {
    cause,
    deliverable: safeText(item.deliverable),
    lead: addressId(item.lead),
    reason: optionalSafeText(item.reason ?? null),
    report: optionalSafeText(item.report ?? null),
    review,
  };
}
function reviewClears(value: unknown): ReviewClear[] {
  return value === undefined
    ? []
    : array(value).map((raw) => {
        const item = record(raw);
        return {
          coordinationId: identity(item.coordination_id),
          deliverable: safeText(item.deliverable),
          by: addressId(item.by),
          at: timestamp(item.at_ms),
        };
      });
}
const noActions: readonly ThreadAction[] = [];
function threadActions(value: unknown): readonly ThreadAction[] {
  if (!Array.isArray(value)) return noActions;
  const found = new Set<ThreadAction>();
  for (const action of value)
    if (action === "continue" || action === "stop") found.add(action);
  return found.size === 0 ? noActions : [...found];
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
    case "terminal_relay":
      return "terminal_relay";
    case "handoff":
      return "handoff";
    case "result_set":
      return "result_set";
    case "nudge":
      return "nudge";
    case "hook":
      return "hook";
    case "reminder":
      return "reminder";
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
      return {
        stage,
        ackedAt: timestamp(item.acked_at_ms),
        answerHeld: item.answer_held === true,
      };
    case "replied":
      return { stage, replyId: identity(item.reply_id) };
    case "completed":
      return { stage, kind: identity(item.kind) };
    default:
      return { stage: "unknown" };
  }
}
const REASON_LIMIT = 60;
// The router model's words, made safe to show on one line.
// The router's topic when the model named none. It says nothing, so it reads as no reason.
const NO_TOPIC = "General topic";
function reasonText(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const line = value.replace(/\s+/g, " ").trim();
  if (line === "" || line === NO_TOPIC) return null;
  const points = Array.from(line);
  return points.length <= REASON_LIMIT
    ? line
    : `${points
        .slice(0, REASON_LIMIT - 1)
        .join("")
        .trimEnd()}…`;
}
function routing(value: unknown): Routing | null {
  if (value === undefined || value === null) return null;
  const item =
    typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  const mentioned = aliases(item.mentioned);
  const to = aliases(item.to);
  switch (item.state) {
    case "routing":
    case "unrouted":
    case "router_failed":
      return { state: item.state, mentioned, to };
    case "routed":
      return {
        state: item.state,
        mentioned,
        to,
        reason: reasonText(item.reason),
        router: optionalIdentity(item.router),
      };
    default:
      return { state: "unknown", mentioned, to };
  }
}
const noAliases: readonly Alias[] = [];
// A list of aliases, bare or with "@", as `Member.alias` holds them. Anything
// else in the list is dropped, so a malformed entry never drops the frame.
function aliases(value: unknown): readonly Alias[] {
  if (!Array.isArray(value)) return noAliases;
  const found = new Set<Alias>();
  for (const raw of value) {
    const alias = optionalAlias(raw);
    if (alias !== null) found.add(alias);
  }
  return found.size === 0 ? noAliases : [...found];
}
const noImages: readonly string[] = [];
// A name that is not a stored image's is dropped, so it never drops the frame.
function imageNames(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return noImages;
  const names = value.filter(isImageName);
  return names.length === 0 ? noImages : names;
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
    progressNote: item.progress_note === true,
    body: safeText(item.body),
    images: imageNames(item.images),
    legs: array(item.legs).map((raw) => {
      const leg = record(raw);
      return { to: addressId(leg.to), stage: deliveryStage(leg.stage) };
    }),
    routing: routing(item.channel),
    woke: aliases(item.woke),
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
export function parseMessageContext(
  raw: unknown,
  target: Conversation,
): MessageContext {
  const item = record(raw);
  const threadId = identity(item.thread_id);
  const rows = [...messages(item.messages).values()].sort(
    (a, b) => a.at - b.at || a.id.localeCompare(b.id),
  );
  const messageCount = integer(item.message_count);
  if (
    threadId !== target.threadId ||
    rows.some((row) => row.threadId !== threadId) ||
    !rows.some((row) => row.id === target.messageId) ||
    rows.length > 102 ||
    messageCount < rows.length ||
    typeof item.earlier_omitted !== "boolean"
  )
    throw new Error("Invalid conversation context");
  return {
    threadId,
    messageCount,
    earlierOmitted: item.earlier_omitted,
    messages: rows,
  };
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
      recentlyClosed: item.recently_closed === true,
      role: optionalSafeText(item.role),
      paneLabel: optionalSafeText(item.pane_label),
      subagents: subagents(item.subagents),
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
    repo: repo(space.repo),
    addressableLabel: nullableText(space.addressable_label),
    notAddressableReason: notAddressableReason(space.not_addressable_reason),
    state: spaceState(space.state),
    health: health(frame.health),
    members,
    work,
    messages: rows,
    threads,
    attention: array(frame.attention).map((item) =>
      attention(item, attentionOccurrences),
    ),
    reviewClears: reviewClears(frame.review_clears),
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
export function parseFullBody(raw: unknown, messageId: string): SafeText {
  const body = record(raw);
  if (identity(body.message_id) !== messageId)
    throw new Error("Full body does not match the message");
  return safeText(body.body);
}
// The server's answer to an upload: the stored name and the line for it.
export function parseImageUpload(raw: unknown): AttachedImage {
  const item = record(raw);
  const { text: rest, images } = splitImages(text(item.line));
  const image = images[0];
  if (
    image === undefined ||
    rest !== "" ||
    images.length !== 1 ||
    image.name !== item.name
  )
    throw new Error("Upload answer does not name one stored image");
  return image;
}
export function parseSessionToken(raw: unknown): string {
  return identity(record(raw).csrf_token);
}
export function parseReminders(raw: unknown): readonly PendingReminder[] {
  const rows = array(record(raw).reminders).map((raw): PendingReminder => {
    const item = record(raw);
    return {
      id: identity(item.reminder_id),
      owner: addressId(item.owner),
      body: safeText(item.body),
      dueAt: item.due_at_ms === null ? null : timestamp(item.due_at_ms),
      repeatsLeft: integer(item.repeats_left),
      repeatUntil:
        item.repeat_until_ms === null ? null : timestamp(item.repeat_until_ms),
    };
  });
  if (new Set(rows.map((row) => row.id)).size !== rows.length)
    throw new Error("Duplicate reminder");
  return rows;
}
function field(answer: unknown, name: string): unknown {
  return typeof answer === "object" && answer !== null && !Array.isArray(answer)
    ? (answer as Record<string, unknown>)[name]
    : undefined;
}
function deliveredList(value: unknown): ReadonlySet<Alias> | null {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string"))
    return null;
  return new Set(value.flatMap((item) => optionalAlias(item) ?? []));
}
// The 2xx status is the server's answer, so a body cut short or not JSON at all
// reads as sent. A JSON body confirms only the agents its `delivered` names.
export function parseDeliverAnswer(
  body: string,
  to: readonly Alias[],
): DeliverAnswer {
  let answer: unknown;
  try {
    answer = JSON.parse(body) as unknown;
  } catch {
    return {
      results: new Map(to.map((alias) => [alias, { kind: "done" }])),
      channel: null,
    };
  }
  const delivered = deliveredList(field(answer, "delivered"));
  const channel = field(answer, "channel");
  return {
    results: new Map(
      to.map((alias): [Alias, DeliverResult] => [
        alias,
        delivered === null
          ? { kind: "failed", cause: "unconfirmed" }
          : delivered.has(alias)
            ? { kind: "done" }
            : { kind: "failed", cause: "left-out" },
      ]),
    ),
    channel:
      typeof channel === "object" && channel !== null && !Array.isArray(channel)
        ? routing(channel)
        : null,
  };
}
export function parseRefusal(body: string): string {
  try {
    const error = record(JSON.parse(body) as unknown).error;
    if (typeof error === "string") return error.trim();
  } catch {}
  return body.trim();
}

export function parseDirectPeers(raw: unknown): readonly DirectPeer[] {
  const peers = array(record(raw).peers).map((raw): DirectPeer => {
    const item = record(raw);
    const presence = item.presence;
    if (presence !== "idle" && presence !== "working" && presence !== "gone")
      throw new Error("Invalid direct message presence");
    if (typeof item.closed !== "boolean")
      throw new Error("Expected closed flag");
    const attentionCount = integer(item.attention_count);
    return {
      spaceKey: spaceKey(item.space_key),
      addressId: addressId(item.address_id),
      alias: `@${identity(item.alias).replace(/^@+/, "")}`,
      workspaceLabel: text(item.workspace_label),
      paneLabel: optionalSafeText(item.pane_label),
      runtime: text(item.runtime),
      activity:
        item.closed || presence === "gone"
          ? "offline"
          : attentionCount > 0
            ? "blocked"
            : presence,
      closed: item.closed,
      threadId: nullableText(item.thread_id),
      lastActivityMs:
        item.last_activity_ms === null
          ? null
          : timestamp(item.last_activity_ms),
      lastMessageId: nullableText(item.last_message_id),
      lastAgentMessageId: nullableText(item.last_agent_message_id),
      attentionCount,
    };
  });
  if (
    new Set(peers.map((peer) => `${peer.spaceKey}/${peer.addressId}`)).size !==
    peers.length
  )
    throw new Error("Duplicate direct message peer");
  return peers;
}
export function parseDirectHistory(raw: unknown): DirectHistory {
  const item = record(raw);
  if (typeof item.earlier_omitted !== "boolean")
    throw new Error("Expected history flag");
  return {
    threadId: nullableText(item.thread_id),
    messages: [...messages(item.messages).values()],
    earlierOmitted: item.earlier_omitted,
  };
}
