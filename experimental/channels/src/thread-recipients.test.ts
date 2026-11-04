import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { homesOf, threadGroups } from "./store.ts";
import {
  threadReplyRecipients,
  threadSkillRecipients,
} from "./thread-recipients.ts";
import { parseEvent } from "./wire.ts";

test("thread defaults use addressable local and scoped mentions, omitting unknown and retired agents", () => {
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
      participants: [agent("local", "@local"), agent("old", "@old", "retired")],
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
    threadReplyRecipients(thread, {
      members: here.members,
      homes: homesOf([here, remote], undefined),
    }),
    [
      { names: ["@local"], mentions: "@local" },
      { names: ["@lead · #beta"], mentions: "#beta @lead" },
    ],
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
