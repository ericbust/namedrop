import assert from "node:assert/strict";
import { test } from "node:test";
import type { AddressId, PeekTarget, SpaceKey } from "./model.ts";
import { peekTarget } from "./peek.ts";
import { sendPeekInput } from "./peekInput.ts";

function lead(): PeekTarget {
  const made = peekTarget("0123456789abcdef0123" as SpaceKey, {
    id: "address-lead" as AddressId,
    alias: "@lead",
    kind: { type: "agent", runtime: "claude" },
    activity: "idle",
    recentlyClosed: false,
    role: null,
    paneLabel: null,
    subagents: [],
  });
  assert.ok(made);
  return made;
}
const input = { kind: "key", key: "Enter", continuity: "c" } as const;

async function answer(status: number, body = ""): Promise<string | null> {
  const original = globalThis.fetch;
  globalThis.fetch = async (url) =>
    String(url) === "/api/session"
      ? new Response(JSON.stringify({ csrf_token: "token" }))
      : new Response(status === 204 ? null : body, { status });
  try {
    await sendPeekInput(lead(), input, new AbortController().signal);
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  } finally {
    globalThis.fetch = original;
  }
}

test("a pane busy with Namedrop's own input says so and that nothing was sent", async () => {
  assert.equal(
    await answer(
      409,
      "Namedrop is typing into this pane right now, so nothing was sent.\n",
    ),
    "Namedrop is typing into @lead right now, so nothing was sent",
  );
});

test("other refusals keep their status text, and a sent input throws nothing", async () => {
  assert.equal(await answer(503), "Input refused (503)");
  assert.equal(await answer(410), "Input refused (410)");
  assert.equal(await answer(204), null);
});
