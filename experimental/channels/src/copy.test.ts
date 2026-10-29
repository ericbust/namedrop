import assert from "node:assert/strict";
import { test } from "node:test";
import {
  attentionCopy,
  avatarHue,
  composerPlaceholder,
  dayBounds,
  dayLabel,
  disabledChannelCopy,
  healthCopy,
  overflowCopy,
  participantCopy,
  pendingCopy,
  presenceWord,
  receiptCopy,
  recipientsLabel,
  relativeAge,
  replyPlaceholder,
  runtimeName,
  sendErrorCopy,
} from "./copy.ts";
import type { Attention, DeliveryStage, Member } from "./model.ts";
import { parseEvent } from "./wire.ts";

test("receipt copy covers every stage and completed kind in plain words", () => {
  const cases: [DeliveryStage, string, string, string, string | undefined][] = [
    [{ stage: "queued" }, "Queued", "clock", "muted", undefined],
    [{ stage: "delivered" }, "Delivered", "check", "muted", undefined],
    [
      { stage: "working", ackedAt: 1 },
      "Working on it",
      "working",
      "working",
      undefined,
    ],
    [
      { stage: "replied", replyId: "r1" },
      "Replied",
      "reply",
      "muted",
      undefined,
    ],
    [
      { stage: "completed", kind: "settled" },
      "Done",
      "done",
      "muted",
      undefined,
    ],
    [
      { stage: "completed", kind: "no_reply" },
      "Done, no reply needed",
      "done",
      "muted",
      undefined,
    ],
    [
      { stage: "completed", kind: "superseded" },
      "Done",
      "done",
      "muted",
      "superseded",
    ],
    [
      { stage: "completed", kind: "constructor" },
      "Done",
      "done",
      "muted",
      "constructor",
    ],
    [{ stage: "held" }, "Held while paused", "pause", "held", undefined],
    [{ stage: "failed" }, "Not delivered", "alert", "failed", undefined],
    [{ stage: "cancelled" }, "Cancelled", "cancelled", "muted", undefined],
    [{ stage: "unknown" }, "Status unknown", "unknown", "muted", undefined],
  ];
  for (const [stage, label, icon, tone, title] of cases) {
    const copy = receiptCopy(stage);
    assert.deepEqual(
      {
        label: copy.label,
        icon: copy.icon,
        tone: copy.tone,
        title: copy.title,
      },
      { label, icon, tone, title },
      JSON.stringify(stage),
    );
    assert.doesNotMatch(copy.label, /settled|no_reply|_/);
  }
});

function attention(
  source: Attention["source"],
  text: string,
  extra: Partial<Attention> = {},
): Attention {
  return {
    rowKey: "row",
    source,
    summary: { text, truncated: false },
    openedAt: 0,
    subjectAlias: null,
    messageId: null,
    ...extra,
  };
}
const ND_DELIVERY =
  "ND-DELIVERY-003: The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox.";

test("Needs-you copy gives every source a title and a hint and keeps the server text in details", () => {
  const table: [Attention["source"], string, string][] = [
    [
      "agent_status",
      "Something needs you",
      "Open its pane in Herdr to answer.",
    ],
    [
      "agent_disposition",
      "Unanswered request",
      "An agent ended its turn without replying.",
    ],
    [
      "delivery_attempt",
      "Unanswered request",
      "An agent ended its turn without replying.",
    ],
    [
      "automatic_reply_limit",
      "Reply limit reached",
      "Continue or stop this thread from its pane.",
    ],
    [
      "coordination",
      "Coordination needs a decision",
      "Review the report and close it from the lead's pane.",
    ],
    [
      "context_grant",
      "Context request waiting",
      "Approve or decline it from the pane.",
    ],
    [
      "pane_binding",
      "An agent's pane changed",
      "Name the pane again to reconnect it.",
    ],
    [
      "integration_registration",
      "Agent setup incomplete",
      "Start a new session in that pane.",
    ],
    ["unknown", "Something needs you", "Open Details for the full message."],
  ];
  for (const [source, title, hint] of table)
    assert.deepEqual(attentionCopy(attention(source, ND_DELIVERY)), {
      title,
      hint,
      details: ND_DELIVERY,
    });
  assert.equal(
    attentionCopy(
      attention("coordination", "Cut", {
        summary: { text: "Cut", truncated: true },
      }),
    ).details,
    "Cut…",
  );
});

test("Needs-you copy names the subject agent for every source when the server sends one", () => {
  const table: [Attention["source"], string, string, string][] = [
    [
      "agent_status",
      "@claude",
      "@claude needs you",
      "Open its pane in Herdr to answer.",
    ],
    [
      "agent_disposition",
      "@codex",
      "@codex didn't answer a request",
      "It ended its turn without replying.",
    ],
    [
      "delivery_attempt",
      "@codex",
      "@codex didn't answer a request",
      "It ended its turn without replying.",
    ],
    [
      "automatic_reply_limit",
      "@codex",
      "@codex hit the reply limit",
      "Continue or stop this thread from its pane.",
    ],
    [
      "coordination",
      "@claude",
      "@claude's coordination needs a decision",
      "Review the report and close it from the lead's pane.",
    ],
    [
      "context_grant",
      "@codex",
      "@codex is asking for context",
      "Approve or decline it from the pane.",
    ],
    [
      "pane_binding",
      "@codex",
      "@codex's pane changed",
      "Name the pane again to reconnect it.",
    ],
    [
      "integration_registration",
      "@codex",
      "@codex isn't set up yet",
      "Start a new session in that pane.",
    ],
    [
      "unknown",
      "@codex",
      "@codex needs you",
      "Open Details for the full message.",
    ],
  ];
  for (const [source, subjectAlias, title, hint] of table)
    assert.deepEqual(
      attentionCopy(attention(source, ND_DELIVERY, { subjectAlias })),
      { title, hint, details: ND_DELIVERY },
    );
});

test("agent status keeps a summary of 90 characters or fewer as the title and moves a longer one to details", () => {
  const hint = "Open its pane in Herdr to answer.";
  const short = "@claude is waiting on an approval or question.";
  const exact = "x".repeat(90);
  const long = "y".repeat(91);
  for (const [subjectAlias, needsYou] of [
    [null, "Something needs you"],
    ["@claude", "@claude needs you"],
  ] as const) {
    for (const kept of [short, exact])
      assert.deepEqual(
        attentionCopy(attention("agent_status", kept, { subjectAlias })),
        { title: kept, hint, details: null },
      );
    assert.deepEqual(
      attentionCopy(attention("agent_status", long, { subjectAlias })),
      { title: needsYou, hint, details: long },
    );
    assert.deepEqual(
      attentionCopy(
        attention("agent_status", short, {
          subjectAlias,
          summary: { text: short, truncated: true },
        }),
      ),
      { title: needsYou, hint, details: `${short}…` },
    );
  }
});

test("health copy reads as words, leaves out zero parts, and never says in flight", () => {
  assert.deepEqual(healthCopy({ level: "calm" }), {
    text: "All quiet",
    tone: "muted",
  });
  const busy: [number, number, string][] = [
    [2, 3, "2 working · 3 pending"],
    [0, 400, "400 pending"],
    [1, 0, "1 working"],
    [0, 0, "All quiet"],
  ];
  for (const [working, inFlight, text] of busy)
    assert.deepEqual(healthCopy({ level: "busy", working, inFlight }), {
      text,
      tone: "muted",
    });
  assert.deepEqual(healthCopy({ level: "paused" }), {
    text: "Paused · deliveries held",
    tone: "held",
  });
  assert.deepEqual(healthCopy({ level: "inactive" }), {
    text: "Not active",
    tone: "muted",
  });
  assert.equal(healthCopy({ level: "unknown" }), null);
  assert.equal(healthCopy({ level: "needs_you", items: 2 }), null);
});

test("presence words, runtime names, ages and avatar hues", () => {
  assert.deepEqual(presenceWord, {
    working: "Working",
    waiting: "Waiting",
    blocked: "Needs you",
    idle: "Idle",
    offline: "Offline",
    retired: "Retired",
    unknown: "Unknown",
  });
  assert.equal(runtimeName("claude-code"), "Claude Code");
  assert.equal(runtimeName("codex"), "Codex");
  assert.equal(runtimeName("opencode"), "OpenCode");
  assert.equal(runtimeName("pi"), "Pi");
  assert.equal(runtimeName("gemini"), "Gemini");
  assert.equal(runtimeName(null), "Agent");
  const now = 10 * 86_400_000;
  assert.equal(relativeAge(now, now), "just now");
  assert.equal(relativeAge(now + 5_000, now), "just now");
  assert.equal(relativeAge(now - 59_999, now), "just now");
  assert.equal(relativeAge(now - 4 * 60_000, now), "4 min");
  assert.equal(relativeAge(now - 2 * 3_600_000 - 1, now), "2 h");
  assert.equal(relativeAge(now - 3 * 86_400_000, now), "3 d");
  assert.equal(avatarHue("@codex"), (64 + 99 + 111 + 100 + 101 + 120) % 6);
  assert.equal(avatarHue(""), 0);
  assert.equal(avatarHue("😀"), (0xd83d + 0xde00) % 6);
});

test("day labels say Today and Yesterday, then a date", () => {
  const now = new Date(2026, 9, 28, 23, 30).getTime();
  const today = dayBounds(now);
  assert.equal(today.start, new Date(2026, 9, 28).getTime());
  assert.equal(today.end, new Date(2026, 9, 29).getTime());
  assert.equal(dayLabel(today.start, now), "Today");
  assert.equal(dayLabel(new Date(2026, 9, 27).getTime(), now), "Yesterday");
  const older = dayLabel(new Date(2026, 9, 20).getTime(), now);
  assert.notEqual(older, "Today");
  assert.notEqual(older, "Yesterday");
  assert.match(older, /20/);
  assert.match(dayLabel(new Date(2025, 9, 20).getTime(), now), /2025/);
});

test("send errors show the server's words for 400, 404 and 409 and fixed words otherwise", () => {
  const generic = "Namedrop couldn't send this. Try again.";
  const cases: [number, string, string][] = [
    [400, "Unknown recipient @ghost", "Unknown recipient @ghost"],
    [404, "Not found.", "Not found."],
    [409, "  Already posted differently \n", "Already posted differently"],
    [400, "   ", generic],
    [
      403,
      "Forbidden.",
      "Your session expired. Reload the page to keep posting.",
    ],
    [413, "Request body is too large.", "This message is too long."],
    [401, "This dashboard needs its key.", generic],
    [500, "internal detail", generic],
    [503, "", generic],
    [0, "", generic],
  ];
  for (const [status, message, shown] of cases)
    assert.equal(
      sendErrorCopy({ kind: "failed", status, message }),
      shown,
      `status ${status}`,
    );
});

test("a queued leg in a paused channel reads Held while paused, other stages keep their words", () => {
  assert.deepEqual(receiptCopy({ stage: "queued" }, true), {
    label: "Held while paused",
    icon: "pause",
    tone: "held",
  });
  assert.equal(receiptCopy({ stage: "queued" }, false).label, "Queued");
  assert.equal(receiptCopy({ stage: "delivered" }, true).label, "Delivered");
  assert.deepEqual(pendingCopy, {
    sending: { label: "Sending…", icon: "clock" },
    sent: { label: "Sent", icon: "check" },
  });
});

test("composer words name the channel, the thread's recipients and the reason a channel is disabled", () => {
  assert.equal(
    composerPlaceholder({ addressableLabel: "alpha", displayName: "alpha" }),
    "Message #alpha",
  );
  assert.equal(
    composerPlaceholder({
      addressableLabel: null,
      displayName: "testbed ui2-live",
    }),
    "Message testbed ui2-live",
  );
  assert.equal(replyPlaceholder(["@codex"]), "Reply to @codex");
  assert.equal(replyPlaceholder([]), "Reply in thread");
  assert.equal(recipientsLabel(["@codex", "@shell"]), "To @codex, @shell");
  assert.deepEqual(disabledChannelCopy, {
    "shared-label":
      "Two workspaces share this label. Rename one in Herdr to mention it.",
    "no-label": "This workspace has no label. Name it in Herdr to mention it.",
  });
  assert.equal(overflowCopy(3), "+3");
});

test("participant names: remote agents carry their home, envoys and unknown ids read as words", () => {
  const event = parseEvent(
    "space",
    JSON.stringify({
      key: "a".repeat(20),
      frame: {
        wire: 1,
        space: {
          key: "a".repeat(20),
          workspace_id: "w1",
          display_name: "alpha",
          addressable_label: "alpha",
        },
        health: { level: "calm" },
        participants: [
          {
            address_id: "pane-1",
            alias: "@codex",
            kind: { type: "agent", runtime: "codex" },
            activity: "idle",
            role: null,
            pane_label: null,
          },
        ],
        messages: [
          {
            message_id: "m1",
            thread_id: "m1",
            in_reply_to: null,
            at_ms: 1,
            author: "envoy:w2:pane-7",
            kind: "mention",
            body: { text: "hi", truncated: false },
            legs: [],
          },
        ],
        threads: [],
        attention: [],
      },
    }),
  );
  assert.ok(event?.type === "space");
  const codex: Member | undefined = event.channel.members.values().next().value;
  const envoy = event.channel.messages.get("m1")?.author;
  assert.ok(codex && envoy !== undefined && envoy !== "human");
  assert.deepEqual(participantCopy({ kind: "operator" }), {
    name: "You",
    suffix: null,
    title: null,
    avatar: "human",
  });
  assert.deepEqual(
    participantCopy({ kind: "member", member: codex, home: "#alpha" }),
    { name: "@codex", suffix: "#alpha", title: null, avatar: "agent" },
  );
  assert.deepEqual(
    participantCopy({ kind: "envoy", id: envoy, home: "#alpha" }),
    {
      name: "an agent in #alpha",
      suffix: null,
      title: "envoy:w2:pane-7",
      avatar: "unresolved",
    },
  );
  assert.equal(
    participantCopy({ kind: "envoy", id: envoy, home: null }).name,
    "an agent in another workspace",
  );
  assert.deepEqual(participantCopy({ kind: "unknown", id: envoy }), {
    name: "an unknown agent",
    suffix: null,
    title: "envoy:w2:pane-7",
    avatar: "unresolved",
  });
});
