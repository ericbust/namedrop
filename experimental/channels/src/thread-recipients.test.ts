import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { homesOf, type ThreadGroup, threadGroups } from "./store.ts";
import { threadSendRule, threadSkillRecipients } from "./thread-recipients.ts";
import { parseEvent } from "./wire.ts";

test("thread rules default to a sole addressable agent and retain unresolved participants", () => {
  const wire = JSON.parse(
    readFileSync(
      new URL("./fixtures/real-frames.jsonl", import.meta.url),
      "utf8",
    )
      .trim()
      .split("\n")[0] ?? "",
  );
  const agent = (id: string, alias: string, activity = "idle") => ({
    address_id: id,
    alias,
    activity,
    kind: { type: "agent", runtime: "codex" },
    role: null,
    pane_label: null,
  });
  const parse = (value: unknown) => {
    const event = parseEvent("space", JSON.stringify(value));
    assert.ok(event?.type === "space");
    return event.channel;
  };
  const here = parse({
    ...wire,
    frame: {
      ...wire.frame,
      participants: [
        agent("local", "@local"),
        agent("old", "@old", "retired"),
        { ...agent("shell", "@shell"), kind: { type: "terminal" } },
      ],
      messages: [
        {
          message_id: "root",
          thread_id: "root",
          in_reply_to: null,
          at_ms: 1,
          author: "human",
          kind: "mention",
          body: { text: "hello", truncated: false },
          legs: [
            "envoy:remote:lead",
            "old",
            "shell",
            "envoy:unknown:agent",
            "local",
            "envoy:remote:lead",
          ].map((to) => ({ to, stage: { stage: "queued" } })),
        },
      ],
    },
  });
  const remote = parse({
    ...wire,
    key: "b".repeat(20),
    frame: {
      ...wire.frame,
      space: {
        ...wire.frame.space,
        key: "b".repeat(20),
        workspace_id: "remote",
        addressable_label: "beta",
      },
      participants: [agent("lead", "@lead")],
      messages: [],
    },
  });
  const thread = threadGroups({
    ...here,
    runs: [[...here.messages.keys()]],
  }).get("root");
  assert.ok(thread);
  assert.deepEqual(
    threadSendRule(thread, {
      members: here.members,
      homes: homesOf([here, remote], undefined),
    }),
    {
      kind: "thread",
      to: ["@lead · #beta", "an agent in another workspace", "@local"],
      implicitRecipient: null,
    },
  );
  const directory = {
    members: here.members,
    homes: homesOf([here, remote], undefined),
  };
  const only = (ids: string[]): ThreadGroup => ({
    ...thread,
    parents: thread.parents.map((row) => ({
      ...row,
      author: "human",
      legs: row.legs.filter((leg) => ids.includes(leg.to)),
    })),
    replies: [],
  });
  assert.equal(
    threadSendRule(only(["local"]), directory).implicitRecipient,
    "@local",
  );
  assert.equal(
    threadSendRule(only(["local", "shell"]), directory).implicitRecipient,
    "@local",
  );
  assert.deepEqual(threadSendRule(only(["shell", "old"]), directory), {
    kind: "thread",
    to: [],
    implicitRecipient: null,
  });
  assert.equal(
    threadSendRule(only(["envoy:remote:lead"]), directory).implicitRecipient,
    "#beta @lead",
  );
  assert.equal(threadSendRule(only([]), directory).implicitRecipient, null);
  assert.equal(
    threadSendRule(only(["envoy:unknown:agent"]), directory).implicitRecipient,
    null,
  );
  assert.equal(
    threadSendRule(only(["local", "envoy:unknown:agent"]), directory)
      .implicitRecipient,
    null,
  );
  const remoteAgent = [...remote.members.values()][0];
  const localAgent = [...here.members.values()].find(
    (member) => member.alias === "@local",
  );
  assert.ok(remoteAgent && localAgent);
  assert.deepEqual(
    threadSkillRecipients(
      thread,
      {
        members: here.members,
        homes: homesOf([here, remote], undefined),
      },
      here.key,
    ),
    [
      { key: remote.key, member: remoteAgent.id },
      { key: here.key, member: localAgent.id },
    ],
    "skill catalogs use the actual remote address and channel, never the envoy or display label",
  );
});
