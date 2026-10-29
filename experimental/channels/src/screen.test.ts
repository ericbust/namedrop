import assert from "node:assert/strict";
import { test } from "node:test";
import type { AddressId, PeekTarget, SpaceKey } from "./model.ts";
import { peekTarget } from "./peek.ts";
import { createScreenReader } from "./screen.ts";

const key = "0123456789abcdef0123" as SpaceKey;
const target = (member: string): PeekTarget => {
  const made = peekTarget(key, {
    id: member as AddressId,
    alias: "@codex",
    kind: { type: "agent", runtime: "codex" },
    activity: "working",
    role: null,
    paneLabel: null,
  });
  assert.ok(made);
  return made;
};
function reader(respond: () => Response | Promise<Response>) {
  const seen: { url: string; init: RequestInit | undefined }[] = [];
  const read = createScreenReader(async (input, init) => {
    seen.push({ url: String(input), init });
    return respond();
  });
  return { seen, read };
}

test("screen reads GET the member's screen in its channel with the session cookie and the caller's signal", async () => {
  const { seen, read } = reader(() => new Response("$ npm test\n144 passed"));
  const controller = new AbortController();
  assert.deepEqual(await read(target("pane 9/x"), controller.signal), {
    kind: "text",
    screen: { earlierOmitted: false, text: "$ npm test\n144 passed" },
  });
  assert.equal(seen.length, 1);
  assert.equal(seen[0]?.url, `/w/${key}/participants/pane%209%2Fx/screen`);
  assert.equal(seen[0]?.init?.credentials, "same-origin");
  assert.equal(seen[0]?.init?.signal, controller.signal);
  assert.equal(seen[0]?.init?.method, undefined);
});
test("screen reads lift the server's omission marker out of the text", async () => {
  const { read } = reader(
    () => new Response("[Earlier screen text omitted]\nnewest line"),
  );
  assert.deepEqual(await read(target("a1"), new AbortController().signal), {
    kind: "text",
    screen: { earlierOmitted: true, text: "newest line" },
  });
});
test("screen reads map each answer to its failure and never reject", async () => {
  for (const [status, failure] of [
    [410, "closed"],
    [404, "missing"],
    [503, "busy"],
    [401, "session"],
    [403, "session"],
    [500, "unreachable"],
    [418, "unreachable"],
  ] as const) {
    const { read } = reader(() => new Response("body", { status }));
    assert.deepEqual(await read(target("a1"), new AbortController().signal), {
      kind: "failed",
      failure,
    });
  }
  const thrown = reader(() => Promise.reject(new TypeError("network down")));
  assert.deepEqual(
    await thrown.read(target("a1"), new AbortController().signal),
    { kind: "failed", failure: "unreachable" },
  );
  const controller = new AbortController();
  controller.abort();
  const aborted = reader(() =>
    Promise.reject(new DOMException("The read was aborted.", "AbortError")),
  );
  assert.deepEqual(await aborted.read(target("a1"), controller.signal), {
    kind: "failed",
    failure: "unreachable",
  });
});
