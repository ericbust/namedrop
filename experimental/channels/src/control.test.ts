import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type ThreadControl,
  threadControl,
  threadControls,
} from "./control.ts";
import type { Attention } from "./model.ts";

test("thread controls show only for a thread the item names, with the actions it lists", () => {
  const item = (
    threadId: string | null,
    actions: Attention["actions"],
  ): Attention => ({
    rowKey: "limit",
    source: "automatic_reply_limit",
    summary: { text: "Paused", truncated: false },
    subjectSummary: null,
    openedAt: 1,
    subjectAlias: "@codex",
    messageId: null,
    threadId,
    actions,
  });
  assert.deepEqual(threadControls(item("t1", ["continue", "stop"])), {
    threadId: "t1",
    actions: ["continue", "stop"],
  });
  assert.deepEqual(threadControls(item("t1", ["stop"])), {
    threadId: "t1",
    actions: ["stop"],
  });
  assert.equal(threadControls(item(null, ["continue", "stop"])), null);
  assert.equal(threadControls(item("t1", [])), null);
});

test("thread control: Stop asks first, a press waits on the frame, and a failure brings the buttons back", () => {
  const idle: ThreadControl = { kind: "idle" };
  const allowing = threadControl(idle, { type: "start", action: "continue" });
  assert.deepEqual(allowing, { kind: "pending", action: "continue" });
  for (const event of [
    { type: "start", action: "continue" },
    { type: "ask-stop" },
    { type: "cancel" },
  ] as const)
    assert.equal(threadControl(allowing, event), allowing, event.type);
  const failed = threadControl(allowing, { type: "failed", status: 500 });
  assert.deepEqual(failed, { kind: "failed", action: "continue", status: 500 });
  assert.deepEqual(
    threadControl(failed, { type: "start", action: "continue" }),
    {
      kind: "pending",
      action: "continue",
    },
  );
  assert.equal(
    threadControl(idle, { type: "start", action: "stop" }),
    idle,
    "Stop never sends without the confirm",
  );
  const asking = threadControl(idle, { type: "ask-stop" });
  assert.deepEqual(asking, { kind: "confirmingStop" });
  assert.deepEqual(threadControl(failed, { type: "ask-stop" }), asking);
  assert.deepEqual(threadControl(asking, { type: "cancel" }), idle);
  assert.equal(
    threadControl(asking, { type: "start", action: "continue" }),
    asking,
  );
  assert.deepEqual(threadControl(asking, { type: "start", action: "stop" }), {
    kind: "pending",
    action: "stop",
  });
  assert.equal(threadControl(idle, { type: "failed", status: 500 }), idle);
});
