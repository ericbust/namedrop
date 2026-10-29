import assert from "node:assert/strict";
import { test } from "node:test";
import {
  attentionCopy,
  avatarHue,
  dayBounds,
  dayLabel,
  healthCopy,
  presenceWord,
  receiptCopy,
  relativeAge,
  runtimeName,
} from "./copy.ts";
import type { Attention, DeliveryStage } from "./model.ts";

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

test("Needs-you copy names the subject agent when the server sends one", () => {
  for (const source of ["delivery_attempt", "agent_disposition"] as const)
    assert.deepEqual(
      attentionCopy(attention(source, ND_DELIVERY, { subjectAlias: "@codex" })),
      {
        title: "@codex didn't answer a request",
        hint: "It ended its turn without replying.",
        details: ND_DELIVERY,
      },
    );
});

test("agent status shows a short summary as the title and moves a long one to details", () => {
  const short = "@claude is waiting on an approval or question.";
  assert.deepEqual(attentionCopy(attention("agent_status", short)), {
    title: short,
    hint: "Open its pane in Herdr to answer.",
    details: null,
  });
  const exact = "x".repeat(90);
  assert.equal(attentionCopy(attention("agent_status", exact)).title, exact);
  const long = "y".repeat(91);
  assert.deepEqual(attentionCopy(attention("agent_status", long)), {
    title: "Something needs you",
    hint: "Open its pane in Herdr to answer.",
    details: long,
  });
  assert.deepEqual(
    attentionCopy(
      attention("agent_status", short, {
        summary: { text: short, truncated: true },
      }),
    ),
    {
      title: "Something needs you",
      hint: "Open its pane in Herdr to answer.",
      details: `${short}…`,
    },
  );
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
