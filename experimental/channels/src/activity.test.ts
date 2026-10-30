import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type ActivityList,
  activityOf,
  activitySummary,
  lineDetail,
  lineText,
} from "./activity.ts";
import type { Channel, Home, SpaceKey, State } from "./model.ts";
import { homesOf, initialState, reduce } from "./store.ts";
import { parseEvent } from "./wire.ts";

const key = (letter: string) => letter.repeat(20);
function lane(
  id: string,
  alias: string,
  activity: string,
  extra: Record<string, unknown> = {},
) {
  return {
    address_id: id,
    alias,
    kind: { type: "agent", runtime: "codex" },
    activity,
    role: null,
    pane_label: null,
    mailbox: [],
    waiting_on: [],
    ...extra,
  };
}
function mail(
  from: string,
  {
    state = "submitted",
    since = 500,
    text = "check the diff",
    truncated = false,
  } = {},
) {
  return {
    delivery_id: `d${since}`,
    message_id: `m${since}`,
    from,
    preview: { text, truncated },
    state,
    since_ms: since,
    attempts: 0,
  };
}
function space(
  letter: string,
  name: string,
  participants: unknown[],
  extra: Record<string, unknown> = {},
) {
  return {
    key: key(letter),
    frame: {
      wire: 1,
      generated_at_ms: 1,
      space: {
        key: key(letter),
        workspace_id: `w-${letter}`,
        display_name: name,
        addressable_label: name,
        state: "active",
      },
      health: { level: "busy", in_flight: 1, working: 1 },
      participants,
      messages: [],
      threads: [],
      attention: [],
      ...extra,
    },
  };
}
function apply(state: State, ...frames: unknown[]): State {
  for (const frame of frames) {
    const event = parseEvent("space", JSON.stringify(frame));
    assert.ok(event, "fixture must parse");
    state = reduce(state, event);
  }
  return state;
}
function load(...frames: unknown[]): ReadonlyMap<SpaceKey, Channel> {
  return apply(initialState(), ...frames).channels;
}
function rows(list: ActivityList) {
  return list.groups.map((group) => [
    group.kind,
    group.rows.map((row) => `${row.member.alias} ${row.channel.displayName}`),
  ]);
}
function line(
  list: ActivityList,
  alias: string,
  homes: ReadonlyMap<string, Home> = new Map(),
) {
  for (const group of list.groups)
    for (const row of group.rows)
      if (row.member.alias === alias) return lineText(row.line, homes);
  assert.fail(`${alias} is not listed`);
}
const message = {
  message_id: "m1",
  thread_id: "m1",
  in_reply_to: null,
  at_ms: 1,
  author: "b1",
  kind: "mention",
  body: { text: "hi", truncated: false },
  legs: [],
};
const blocker = {
  source_kind: "agent_status",
  summary: {
    text: "Waiting for permission to run a command",
    truncated: false,
  },
  opened_at_ms: 900,
  subject_alias: "claude",
  message_id: null,
};

test("activity rows: only working, waiting and blocked agents and terminals, never the operator", () => {
  const list = activityOf(
    load(
      space("a", "alpha", [
        { ...lane("human", "you", "waiting"), kind: { type: "human" } },
        lane("a1", "@idle", "idle"),
        lane("a2", "@off", "offline"),
        lane("a3", "@gone", "retired"),
        lane("a4", "@odd", "future_activity"),
        { ...lane("a5", "@shell", "working"), kind: { type: "terminal" } },
        lane("a6", "@codex", "working"),
      ]),
    ),
  );
  assert.deepEqual(rows(list), [["working", ["@codex alpha", "@shell alpha"]]]);
  assert.equal(list.count, 2);
  assert.deepEqual(
    activityOf(load(space("a", "alpha", [lane("a1", "@idle", "idle")]))).groups,
    [],
  );
});

test("activity line 2: attention about the member, then waiting on others, then the oldest mail, then nothing", () => {
  const list = activityOf(
    load(
      space(
        "a",
        "alpha",
        [
          lane("a1", "@claude", "blocked", {
            waiting_on: ["@codex"],
            mailbox: [mail("@lead")],
          }),
          lane("a2", "@ask", "blocked", {
            waiting_on: ["@codex", "you", "@shell"],
            mailbox: [mail("@lead")],
          }),
          lane("a3", "@lead", "waiting", { mailbox: [mail("you")] }),
          lane("a4", "@codex", "working", {
            mailbox: [
              mail("@lead", {
                since: 400,
                text: "a long ask",
                truncated: true,
              }),
              mail("@shell", { since: 300 }),
            ],
          }),
          lane("a5", "@quiet", "working"),
        ],
        {
          attention: [
            {
              ...blocker,
              opened_at_ms: 100,
              summary: { text: "older", truncated: false },
            },
            blocker,
            { ...blocker, opened_at_ms: 950, subject_alias: "lead" },
          ],
        },
      ),
    ),
  );
  assert.equal(
    line(list, "@claude"),
    "Waiting for permission to run a command",
  );
  assert.equal(line(list, "@ask"), "Waiting on @codex, you +1");
  assert.equal(line(list, "@lead"), "From you: “check the diff”");
  assert.equal(line(list, "@codex"), "From @lead: “a long ask…”");
  assert.equal(line(list, "@quiet"), "");
});

test("member activity uses the subject summary and keeps older and stored text intact", () => {
  const full = "@claude is waiting on an approval or question.";
  for (const [extra, expected] of [
    [
      {
        subject_summary: {
          text: "Waiting on an approval or question.",
          truncated: false,
        },
      },
      "Waiting on an approval or question.",
    ],
    [{ subject_summary: { text: "Waiting", truncated: true } }, "Waiting…"],
    [{}, full],
    [{ subject_summary: null }, full],
  ] satisfies [Record<string, unknown>, string][]) {
    const list = activityOf(
      load(
        space("a", "alpha", [lane("a1", "@claude", "blocked")], {
          attention: [
            { ...blocker, summary: { text: full, truncated: false }, ...extra },
          ],
        }),
      ),
    );
    assert.equal(line(list, "@claude"), expected);
  }
});

test("activity rows: an item the server describes in its own terms reads in plain words, with the server's text in the tooltip", () => {
  const server =
    "Coordination coordination-1793308769836090626-2129769-1 needs a decision: the report is saved and waits for the lead.";
  for (const [source, expected] of [
    ["coordination", "Coordination needs a decision"],
    ["agent_disposition", "Unanswered request"],
    ["delivery_attempt", "Unanswered request"],
    ["automatic_reply_limit", "Reply limit reached"],
    ["context_grant", "Context request waiting"],
    ["pane_binding", "An agent's pane changed"],
    ["integration_registration", "Agent setup incomplete"],
  ] satisfies [string, string][]) {
    const list = activityOf(
      load(
        space("a", "alpha", [lane("a1", "@claude", "blocked")], {
          attention: [
            {
              ...blocker,
              source_kind: source,
              summary: { text: server, truncated: false },
            },
          ],
        }),
      ),
    );
    const row = list.groups
      .flatMap((group) => group.rows)
      .find((r) => r.member.alias === "@claude");
    assert.ok(row, source);
    assert.equal(lineText(row.line, new Map()), expected, source);
    assert.equal(lineDetail(row.line), server, source);
  }
  const list = activityOf(
    load(
      space("a", "alpha", [lane("a1", "@claude", "blocked")], {
        attention: [blocker],
      }),
    ),
  );
  const row = list.groups.flatMap((group) => group.rows)[0];
  assert.ok(row);
  assert.equal(
    lineText(row.line, new Map()),
    "Waiting for permission to run a command",
    "an agent's own words stay",
  );
  assert.equal(lineDetail(row.line), null);
});

test("activity rows: held marks the oldest item when it is held, or queued in a paused channel", () => {
  const held = (state: string, level: string) => {
    const list = activityOf(
      load(
        space(
          "a",
          "alpha",
          [
            lane("a1", "@codex", "working", {
              mailbox: [mail("@lead", { state })],
            }),
          ],
          {
            health: { level },
          },
        ),
      ),
    );
    const row = list.groups[0]?.rows[0];
    assert.ok(row?.line.kind === "mail");
    return row.line.held;
  };
  assert.equal(held("held", "calm"), true);
  assert.equal(held("queued", "paused"), true);
  assert.equal(held("dispatching", "paused"), true);
  assert.equal(held("submitted", "paused"), false);
  assert.equal(held("queued", "calm"), false);
});

test("activity order: oldest mail first, rows with none last, then channel name and alias", () => {
  const list = activityOf(
    load(
      space("c", "gamma", [
        lane("c1", "@new", "working", {
          mailbox: [mail("@lead", { since: 900 })],
        }),
        lane("c2", "@none", "working"),
      ]),
      space("b", "beta", [
        lane("b2", "@zed", "working"),
        lane("b1", "@amy", "working"),
        lane("b3", "@old", "working", {
          mailbox: [mail("@lead", { since: 100 })],
        }),
      ]),
      space("a", "alpha", [lane("a1", "@zed", "working")]),
    ),
  );
  assert.deepEqual(rows(list), [
    [
      "working",
      [
        "@old beta",
        "@new gamma",
        "@zed alpha",
        "@amy beta",
        "@zed beta",
        "@none gamma",
      ],
    ],
  ]);
  assert.deepEqual(
    list.groups[0]?.rows.map((row) => row.since),
    [100, 900, null, null, null, null],
  );
});

test("activity names: agents in other workspaces read through their home channel, never as a raw envoy id", () => {
  const home = space("b", "beta", [lane("pane-9", "@reviewer", "idle")]);
  const channels = load(
    space("a", "alpha", [
      lane("a1", "@codex", "waiting", {
        waiting_on: ["@envoy:w-b:pane-9", "@envoy:w-b:pane-gone"],
        mailbox: [mail("@envoy:w-z:pane-1")],
      }),
      lane("a2", "@lead", "working", { mailbox: [mail("@envoy:w-b:pane-9")] }),
    ]),
    home,
  );
  const homes = homesOf(channels.values(), undefined);
  const list = activityOf(channels);
  assert.equal(
    line(list, "@codex", homes),
    "Waiting on @reviewer · #beta, an agent in #beta",
  );
  assert.equal(
    line(list, "@lead", homes),
    "From @reviewer · #beta: “check the diff”",
  );
  const unresolved = activityOf(
    load(
      space("a", "alpha", [
        lane("a1", "@codex", "working", {
          mailbox: [mail("@envoy:w-z:pane-1")],
        }),
      ]),
    ),
  );
  assert.equal(
    line(unresolved, "@codex"),
    "From an agent in another workspace: “check the diff”",
  );
});

test("activity reuse: a frame that changes nothing returns the same list, and a change keeps every other row", () => {
  const alpha = space("a", "alpha", [
    lane("a1", "@codex", "working", { mailbox: [mail("@lead")] }),
  ]);
  const beta = space("b", "beta", [
    lane("b1", "@lead", "waiting", { waiting_on: ["@codex"] }),
  ]);
  const state = apply(initialState(), alpha, beta);
  const first = activityOf(state.channels);
  const talked = apply(state, {
    ...beta,
    frame: { ...beta.frame, messages: [message] },
  });
  assert.notEqual(talked.channels, state.channels);
  assert.equal(activityOf(talked.channels, first), first);
  const acked = space("a", "alpha", [
    lane("a1", "@codex", "working", {
      mailbox: [mail("@lead", { state: "acknowledged", since: 700 })],
    }),
  ]);
  const next = activityOf(apply(talked, acked).channels, first);
  assert.notEqual(next, first);
  assert.equal(
    next.groups[0],
    first.groups[0],
    "the waiting group is untouched",
  );
  assert.notEqual(next.groups[1]?.rows[0], first.groups[1]?.rows[0]);
  assert.equal(next.groups[1]?.rows[0]?.since, 700);
});

test("activity summary: one count per group, leaving out zeros", () => {
  const list = activityOf(
    load(
      space("a", "alpha", [
        lane("a1", "@codex", "working"),
        lane("a2", "@lead", "working"),
      ]),
      space("b", "beta", [
        lane("b1", "@claude", "blocked"),
        lane("b2", "@ask", "waiting"),
      ]),
    ),
  );
  assert.equal(activitySummary(list), "1 needs you · 1 waiting · 2 working");
  assert.equal(
    activitySummary(
      activityOf(load(space("a", "alpha", [lane("a1", "@codex", "working")]))),
    ),
    "1 working",
  );
});
