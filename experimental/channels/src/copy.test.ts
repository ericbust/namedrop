import assert from "node:assert/strict";
import { test } from "node:test";
import {
  attentionCopy,
  avatarHue,
  composerCopy,
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
  referenceCopy,
  relativeAge,
  repliesCopy,
  replyLabel,
  replyPlaceholder,
  responseCopy,
  routeCopy,
  runtimeName,
  sendErrorCopy,
  threadControlCopy,
} from "./copy.ts";
import type {
  AddressId,
  Attention,
  DeliveryStage,
  Member,
  MemberKind,
  Participant,
} from "./model.ts";
import { parseEvent } from "./wire.ts";

function recipient(kind: MemberKind): Participant {
  return {
    kind: "member",
    member: {
      id: "id-x" as Member["id"],
      alias: "@x",
      kind,
      activity: "idle",
      recentlyClosed: false,
      role: null,
      paneLabel: null,
    },
    home: null,
  };
}
const agentRecipient = recipient({ type: "agent", runtime: "codex" });

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
      "Ended without an answer",
      "unknown",
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
    const copy = receiptCopy(stage, agentRecipient);
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

test("a settled turn reads Ended without an answer for an agent, and Done for a terminal whose command finished", () => {
  const settled: DeliveryStage = { stage: "completed", kind: "settled" };
  const ended = {
    label: "Ended without an answer",
    icon: "unknown",
    tone: "muted",
  };
  const done = { label: "Done", icon: "done", tone: "muted" };
  const cases: [string, Participant, object][] = [
    ["an agent", agentRecipient, ended],
    [
      "an agent with no runtime",
      recipient({ type: "agent", runtime: null }),
      ended,
    ],
    [
      "an agent in another workspace",
      { kind: "envoy", id: "envoy:w2:x" as Member["id"], home: null },
      ended,
    ],
    [
      "an id the page cannot place",
      { kind: "unknown", id: "x" as Member["id"] },
      ended,
    ],
    ["a terminal", recipient({ type: "terminal" }), done],
    [
      "a member of a kind the page does not know",
      recipient({ type: "unknown" }),
      done,
    ],
    ["the operator", { kind: "operator" }, done],
  ];
  for (const [who, to, shown] of cases)
    assert.deepEqual(receiptCopy(settled, to), shown, who);
  for (const to of [agentRecipient, recipient({ type: "terminal" })])
    assert.equal(
      receiptCopy({ stage: "completed", kind: "no_reply" }, to).label,
      "Done, no reply needed",
    );
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
    subjectSummary: null,
    openedAt: 0,
    subjectAlias: null,
    messageId: null,
    threadId: null,
    actions: [],
    ...extra,
  };
}
const ND_DELIVERY =
  "ND-DELIVERY-003: The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox.";

test("a reply-limit item the browser can continue or stop says why the agents paused, and names each control in plain words", () => {
  const limit = (extra: Partial<Attention>) =>
    attentionCopy(attention("automatic_reply_limit", "Paused", extra));
  const paused = "The agents paused after 10 replies in a row.";
  assert.equal(
    limit({ threadId: "t1", actions: ["continue", "stop"] }).hint,
    paused,
  );
  assert.equal(limit({ threadId: "t1", actions: ["stop"] }).hint, paused);
  assert.deepEqual(
    limit({ threadId: "t1", actions: ["continue"], subjectAlias: "@codex" }),
    {
      title: "@codex hit the reply limit",
      hint: paused,
      hintTitle: null,
      details: "Paused",
    },
  );
  for (const extra of [{}, { threadId: "t1" }, { actions: ["continue"] }])
    assert.equal(
      limit(extra as Partial<Attention>).hint,
      "Continue or stop this thread from its pane.",
      JSON.stringify(extra),
    );
  const { status, ...words } = threadControlCopy;
  assert.equal(status(404), "HTTP 404");
  assert.deepEqual(words, {
    continue: {
      label: "Allow 10 more replies",
      pending: "Allowing…",
      title: "Lets the agents send 10 more replies in this thread.",
      failed: "Couldn't allow more replies. Try again.",
    },
    stop: {
      label: "Stop thread",
      pending: "Stopping…",
      title: "Ends agent replies in this thread. You can't undo this.",
      failed: "Couldn't stop the thread. Try again.",
    },
    confirm: "Stop this thread for good?",
    confirmStop: "Stop",
    cancel: "Cancel",
    gone: "This thread is no longer in this workspace.",
  });
});

test("Needs-you copy gives every source a title and a hint and keeps the server text in details", () => {
  const table: [Attention["source"], string, string][] = [
    ["agent_status", "Something needs you", "Answer it in its pane in Herdr."],
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
      "Read the report, then answer the lead.",
    ],
    [
      "context_grant",
      "Context request waiting",
      "Approve or decline it in the agent's pane in Herdr.",
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
      hintTitle: null,
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
      "Answer it in @claude's pane in Herdr.",
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
      "Read the report, then answer the lead.",
    ],
    [
      "context_grant",
      "@codex",
      "@codex is asking for context",
      "Approve or decline it in the agent's pane in Herdr.",
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
      { title, hint, hintTitle: null, details: ND_DELIVERY },
    );
});

test("agent status keeps a summary of 90 characters or fewer as the title and moves a longer one to details", () => {
  const short = "@claude is waiting on an approval or question.";
  const exact = "x".repeat(90);
  const long = "y".repeat(91);
  for (const [subjectAlias, needsYou, hint] of [
    [null, "Something needs you", "Answer it in its pane in Herdr."],
    ["@claude", "@claude needs you", "Answer it in @claude's pane in Herdr."],
  ] as const) {
    for (const kept of [short, exact])
      assert.deepEqual(
        attentionCopy(attention("agent_status", kept, { subjectAlias })),
        { title: kept, hint, hintTitle: null, details: null },
      );
    assert.deepEqual(
      attentionCopy(attention("agent_status", long, { subjectAlias })),
      { title: needsYou, hint, hintTitle: null, details: long },
    );
    assert.deepEqual(
      attentionCopy(
        attention("agent_status", short, {
          subjectAlias,
          summary: { text: short, truncated: true },
        }),
      ),
      { title: needsYou, hint, hintTitle: null, details: `${short}…` },
    );
  }
});

function agent(alias: string, pane: string | null = null): Member {
  return {
    id: "a" as AddressId,
    alias,
    kind: { type: "agent", runtime: "codex" },
    activity: "idle",
    recentlyClosed: false,
    role: null,
    paneLabel: pane === null ? null : { text: pane, truncated: false },
  };
}
test("with a pane response, the hint names the responder's pane with its label as the tooltip, except where the source's own hint wins", () => {
  const pane = { kind: "pane", addressId: "a" as AddressId } as const;
  const worker = agent("@worker", "w1:worker");
  const copy = (
    source: Attention["source"],
    extra: Partial<Attention> = {},
  ) => {
    const { hint, hintTitle } = attentionCopy(
      attention(source, "Waiting", { response: pane, ...extra }),
      worker,
    );
    return { hint, hintTitle };
  };
  const answer = {
    hint: "Answer it in @worker's pane in Herdr.",
    hintTitle: "w1:worker",
  };
  assert.deepEqual(copy("context_grant"), {
    hint: "Approve or decline it in @worker's pane in Herdr.",
    hintTitle: "w1:worker",
  });
  assert.deepEqual(
    copy("automatic_reply_limit", {
      threadId: "t1",
      actions: ["continue", "stop"],
    }),
    {
      hint: "The agents paused after 10 replies in a row.",
      hintTitle: null,
    },
    "the paused hint stands beside Continue and Stop, with no pane tooltip",
  );
  assert.deepEqual(
    copy("automatic_reply_limit"),
    answer,
    "without browser controls the pane is the way to answer",
  );
  for (const source of [
    "agent_status",
    "agent_disposition",
    "delivery_attempt",
    "coordination",
    "pane_binding",
    "integration_registration",
    "unknown",
  ] as const)
    assert.deepEqual(copy(source), answer, source);
  assert.deepEqual(
    {
      subject: attentionCopy(
        attention("context_grant", "Waiting", {
          response: pane,
          subjectAlias: "@codex",
        }),
      ),
      nobody: attentionCopy(attention("context_grant", "Waiting")).hint,
    },
    {
      subject: {
        title: "@codex is asking for context",
        hint: "Approve or decline it in @codex's pane in Herdr.",
        hintTitle: null,
        details: "Waiting",
      },
      nobody: "Approve or decline it in the agent's pane in Herdr.",
    },
    "the subject names the pane when the responder is unknown, and with no one known the table row stands",
  );
});
test("a pane response's hint names the pane to answer in, a local response's hint is its setup step, and the table hint stays for the rest", () => {
  const pane = { kind: "pane", addressId: "a" as AddressId } as const;
  const hint = (
    source: Attention["source"],
    extra: Partial<Attention>,
    responder: string | null = null,
  ) =>
    attentionCopy(
      attention(source, "Waiting", extra),
      responder === null ? null : agent(responder),
    ).hint;
  for (const source of ["agent_status", "pane_binding"] as const) {
    assert.equal(
      hint(source, { response: pane }, "@worker"),
      "Answer it in @worker's pane in Herdr.",
      source,
    );
    assert.equal(
      hint(source, { response: pane, subjectAlias: "@codex" }, "@worker"),
      "Answer it in @worker's pane in Herdr.",
      "the responder wins over the subject",
    );
    assert.equal(
      hint(source, { response: pane, subjectAlias: "@codex" }),
      "Answer it in @codex's pane in Herdr.",
    );
  }
  assert.equal(
    hint("context_grant", { response: pane }),
    "Approve or decline it in the agent's pane in Herdr.",
  );
  assert.equal(
    hint("integration_registration", { response: { kind: "local" } }),
    "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration.",
  );
  assert.equal(
    hint("coordination", {
      response: { kind: "local" },
      subjectAlias: "@codex",
    }),
    "Open this workspace in Herdr and choose Namedrop Doctor to review this item.",
  );
  assert.equal(
    hint(
      "coordination",
      { response: { kind: "message", addressId: "a" as AddressId } },
      "@claude",
    ),
    "Read the report, then answer the lead.",
  );
  assert.equal(
    hint(
      "delivery_attempt",
      { response: { kind: "message", addressId: "a" as AddressId } },
      "@quiet",
    ),
    "An agent ended its turn without replying.",
  );
  assert.deepEqual(
    {
      reply: responseCopy.reply("@quiet"),
      message: responseCopy.message("@claude"),
      pane: responseCopy.pane("@worker"),
    },
    {
      reply: "Reply to @quiet",
      message: "Message @claude",
      pane: "See @worker's screen",
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
    retired: "Pane closed",
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

test("send errors show the server's words as a sentence for 400, 404 and 409 and fixed words otherwise", () => {
  const generic = "Namedrop couldn't send this. Try again.";
  const expired = "Your session expired. Reload the page to keep posting.";
  const cases: [number, string, string][] = [
    [
      400,
      "message must begin with one or more recipient Mentions",
      "Message must begin with one or more recipient Mentions.",
    ],
    [400, "Unknown recipient @ghost", "Unknown recipient @ghost."],
    [400, "@ghost is not here", "@ghost is not here."],
    [404, "Not found.", "Not found."],
    [409, "  already posted differently? \n", "Already posted differently?"],
    [400, "   ", generic],
    [403, "Forbidden.", expired],
    [413, "Request body is too large.", "This message is too long."],
    [401, "This dashboard needs its key.", expired],
    [500, "internal detail", generic],
    [503, "", generic],
    [0, "", generic],
  ];
  for (const [status, message, shown] of cases)
    assert.equal(sendErrorCopy({ status, message }), shown, `status ${status}`);
});

test("a queued leg in a paused channel reads Held while paused, other stages keep their words", () => {
  assert.deepEqual(receiptCopy({ stage: "queued" }, agentRecipient, true), {
    label: "Held while paused",
    icon: "pause",
    tone: "held",
  });
  assert.equal(
    receiptCopy({ stage: "queued" }, agentRecipient, false).label,
    "Queued",
  );
  assert.equal(
    receiptCopy({ stage: "delivered" }, agentRecipient, true).label,
    "Delivered",
  );
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
  assert.equal(replyPlaceholder(["@codex"]), "Write a reply");
  assert.equal(replyPlaceholder(["@lead · #alpha"]), "Write a reply");
  assert.equal(replyLabel(["@codex"]), "Reply to @codex");
  assert.equal(replyLabel(["@lead · #alpha"]), "Reply to @lead · #alpha");
  const noOne = "Start with @name to choose who gets this.";
  assert.equal(replyPlaceholder([]), noOne);
  assert.equal(replyLabel([]), noOne);
  assert.equal(recipientsLabel(["@codex", "@shell"]), "To @codex, @shell");
  assert.equal(composerCopy.needsRecipient, noOne);
  assert.equal(composerCopy.hint, "Enter to send · Shift+Enter for a new line");
  assert.equal(
    composerCopy.toRoom,
    "Enter posts to the channel · Mention @name to bring someone in",
  );
  assert.equal(
    composerCopy.checks(["@codex"]),
    "Enter posts to the channel · Namedrop checks if it's for @codex",
  );
  assert.equal(
    composerCopy.checks(["@codex", "@claude", "@lead"]),
    "Enter posts to the channel · Namedrop checks if it's for @codex, @claude +1",
  );
  assert.equal(
    composerCopy.sendsOnlyTo(["@codex"]),
    "Enter sends only to @codex",
  );
  assert.equal(
    composerCopy.sendsOnlyTo(["@codex", "@shell"]),
    "Enter sends only to @codex, @shell",
  );
  assert.equal(
    composerCopy.sendsOnlyTo(["@codex", "@shell", "@a", "@b"]),
    "Enter sends only to @codex, @shell +2",
  );
  assert.deepEqual(disabledChannelCopy, {
    "unusable-label":
      "This label has no Latin letters or digits, so Namedrop can't mention it. Rename it in Herdr.",
    "no-label": "This workspace has no label in Herdr. Name it to mention it.",
    "shared-name":
      "Another workspace shows the same name. Give one a unique label in Herdr to mention it.",
    unknown:
      "Namedrop can't mention this workspace by its label. Check that the label is unique and isn't “all”, and that Namedrop is active there.",
  });
  assert.equal(composerCopy.replyNotSent, "Reply not sent · Open thread");
  assert.equal(composerCopy.postNotSent, "A post wasn't sent");
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
  const alpha = {
    key: event.channel.key,
    name: "#alpha",
    token: "#alpha",
    members: new Map(),
  };
  assert.deepEqual(participantCopy({ kind: "operator" }), {
    name: "You",
    suffix: null,
    title: null,
    avatar: "human",
  });
  assert.deepEqual(
    participantCopy({ kind: "member", member: codex, home: alpha }),
    { name: "@codex", suffix: "#alpha", title: null, avatar: "agent" },
  );
  assert.deepEqual(participantCopy({ kind: "envoy", id: envoy, home: alpha }), {
    name: "an agent in #alpha",
    suffix: null,
    title: "envoy:w2:pane-7",
    avatar: "unresolved",
  });
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

test("channel chat words: the route line, its offers and tooltips, reply counts and the reference line", () => {
  assert.deepEqual(routeCopy.state, {
    checking: "Checking who this is for…",
    failed: "Couldn't check who this is for",
  });
  assert.equal(routeCopy.pulledIn, "pulled in");
  assert.equal(routeCopy.mentioned(["@codex"]), "Mentioned @codex, not sent");
  assert.equal(
    routeCopy.mentioned(["@codex", "@claude"]),
    "Mentioned 2 agents, not sent",
  );
  assert.deepEqual(routeCopy.send, { anyway: "Send anyway", to: "Send to" });
  assert.equal(routeCopy.sendThis("@codex"), "Send this to @codex");
  assert.equal(routeCopy.tip.router("claude-haiku"), "Picked by claude-haiku");
  assert.equal(
    routeCopy.tip.unreachable,
    "Namedrop couldn't reach its router, so it didn't pick who gets this post. The post is saved.",
  );
  assert.equal(repliesCopy(1), "1 reply");
  assert.equal(repliesCopy(3), "3 replies");
  assert.equal(routeCopy.openReplies(3), "3 replies. Open thread");
  assert.equal(
    referenceCopy.label("@claude", "The arm64 link step fails."),
    "Reply to @claude: The arm64 link step fails. Open thread",
  );
  assert.equal(
    referenceCopy.label("@claude", `${"a".repeat(78)} ${"b".repeat(9)}`),
    `Reply to @claude: ${"a".repeat(78)}… Open thread`,
    "a long first line is clipped in the accessible name",
  );
  assert.equal(
    referenceCopy.label("You", "can you look"),
    "Reply to You: can you look. Open thread",
  );
  assert.equal(referenceCopy.earlier, "Reply to an earlier message");
  assert.equal(
    referenceCopy.earlierLabel,
    "Reply to an earlier message. Open thread",
  );
});
