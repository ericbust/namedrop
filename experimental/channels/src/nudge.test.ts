import assert from "node:assert/strict";
import { test } from "node:test";
import { plainNudge } from "./nudge.ts";

test("a coordination nudge reads in plain words: this coordination, then numbered assignments with spaced outcomes", () => {
  assert.equal(
    plainNudge(
      "Coordination coordination-1793308769836090626-2129769-1 (revision 3) remains open. Deliverable: CL_A_LIVE_DECISION. Review the current assignments and take an explicit next action or save a final report.\nAssignment assignment-1793308772078455575-2130363-4 revision 4: needs_continuation.\nAssignment assignment-1793308772078455575-2130363-7 revision 1: completed.",
    ),
    "This coordination (revision 3) remains open. Deliverable: CL_A_LIVE_DECISION. Review the current assignments and take an explicit next action or save a final report.\nAssignment 1, revision 4: needs continuation.\nAssignment 2, revision 1: completed.",
  );
});

test("an assignment named twice keeps its first number", () => {
  assert.equal(
    plainNudge(
      "Assignment assignment-9-9-1 revision 2: blocked_on_review.\nAssignment assignment-9-9-2 revision 1: completed.\nAssignment assignment-9-9-1 revision 3: completed.",
    ),
    "Assignment 1, revision 2: blocked on review.\nAssignment 2, revision 1: completed.\nAssignment 1, revision 3: completed.",
  );
});

test("every other bare id is removed with the space around it", () => {
  for (const [raw, plain] of [
    ["Waiting on coordination-1-2-3 now.", "Waiting on now."],
    ["See assignment-1-2-3, then answer.", "See, then answer."],
    ["message-10-20-30 is late", "is late"],
    ["Thread thread-1-2-3", "Thread"],
    ["Retry delivery-1-2-3 soon", "Retry soon"],
    ["Retry attempt-1-2-3 soon", "Retry soon"],
    ["Retry delivery-attempt-1-2-3 soon", "Retry soon"],
    ["Answer request-1-2-3 and request-4-5-6 today", "Answer and today"],
    ["one\nmessage-1-2-3\ntwo", "one\n\ntwo"],
  ] satisfies [string, string][])
    assert.equal(plainNudge(raw), plain, raw);
});

test("text with no tracked id is unchanged, including look-alikes", () => {
  for (const text of [
    "Please continue",
    "Remind @codex about the build.  Two spaces stay.",
    "message-12 and snapshot-1-2-3 and coordination-1-2 stay",
    "premessage-1-2-3 stays",
    "coordination-1-2-3-4x stays",
  ])
    assert.equal(plainNudge(text), text);
});
