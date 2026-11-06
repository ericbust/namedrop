import assert from "node:assert/strict";
import { test } from "node:test";
import { reminderRepeats } from "./reminders.ts";
import { parseReminders } from "./wire.ts";

const wireReminder = {
  reminder_id: "r1",
  owner: "agent",
  body: { text: "Check the build", truncated: false },
  due_at_ms: 60000,
  repeats_left: 2,
  repeat_until_ms: null,
};

test("pending reminder parsing preserves owner, due time, body and repeats", () => {
  const [row] = parseReminders({ reminders: [wireReminder] });
  assert.ok(row);
  assert.equal(row.owner, "agent");
  assert.equal(row.dueAt, 60000);
  assert.equal(row.body.text, "Check the build");
  assert.equal(reminderRepeats(row), "2 repeats left");
  assert.match(
    reminderRepeats({ ...row, repeatUntil: 120000, repeatsLeft: 99 }),
    /up to 99 repeats left/,
  );
  assert.throws(() =>
    parseReminders({ reminders: [{ ...wireReminder, repeats_left: -1 }] }),
  );
  assert.throws(() =>
    parseReminders({ reminders: [wireReminder, wireReminder] }),
  );
});

test("one-time reminders omit repeat copy while future repeats keep their count", () => {
  const [row] = parseReminders({
    reminders: [{ ...wireReminder, repeats_left: 0 }],
  });
  assert.ok(row);
  assert.equal(reminderRepeats(row), null);
  assert.equal(reminderRepeats({ ...row, repeatsLeft: 1 }), "1 repeat left");
  assert.equal(reminderRepeats({ ...row, repeatsLeft: 2 }), "2 repeats left");
  assert.match(
    reminderRepeats({ ...row, repeatUntil: 120000 }) ?? "",
    /Repeats until .* · up to 0 repeats left/,
  );
});
