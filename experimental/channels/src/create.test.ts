import assert from "node:assert/strict";
import { test } from "node:test";
import { createCopy } from "./copy.ts";
import {
  agentRequest,
  channelRequest,
  editing,
  type FormState,
  formState,
  liveNameProblem,
  takenNames,
} from "./create.ts";

const problem = (message: string) => ({ kind: "problem", message });

test("new channel: a draft is trimmed and sent once its name and folder pass the server's own checks", () => {
  assert.deepEqual(
    channelRequest({ name: "  Proto Chan ", folder: " ~/proj " }),
    { kind: "ok", request: { name: "Proto Chan", folder: "~/proj" } },
  );
  assert.deepEqual(channelRequest({ name: "web", folder: "/srv/web" }), {
    kind: "ok",
    request: { name: "web", folder: "/srv/web" },
  });
  assert.deepEqual(channelRequest({ name: "x".repeat(64), folder: "~" }), {
    kind: "ok",
    request: { name: "x".repeat(64), folder: "~" },
  });
});

test("new channel: each refusal reads the server's sentence, name checks before folder", () => {
  const cases: [string, string, string][] = [
    ["   ", "/srv", createCopy.channelNameMissing],
    ["x".repeat(65), "/srv", createCopy.channelNameLong],
    ["ü".repeat(65), "/srv", createCopy.channelNameLong],
    ["all", "/srv", createCopy.channelNameUnusable],
    ["ALL", "/srv", createCopy.channelNameUnusable],
    ["!!!", "/srv", createCopy.channelNameUnusable],
    ["--", "/srv", createCopy.channelNameUnusable],
    ["._.", "/srv", createCopy.channelNameUnusable],
    ["web\u0007", "/srv", createCopy.channelNameHidden],
    ["web​app", "/srv", createCopy.channelNameHidden],
    ["web\u{E0041}", "/srv", createCopy.channelNameHidden],
    ["web", "", createCopy.folderNotPath],
    ["web", "projects/web", createCopy.folderNotPath],
    ["", "projects/web", createCopy.channelNameMissing],
  ];
  for (const [name, folder, message] of cases)
    assert.deepEqual(
      channelRequest({ name, folder }),
      problem(message),
      JSON.stringify(name),
    );
  assert.equal(
    createCopy.channelNameUnusable,
    'A channel name needs a letter or a digit, and it can\'t be "all".',
  );
});

test("add agent: the name loses one leading @, a blank persona is none, and the kind passes through", () => {
  const none = new Set<string>();
  assert.deepEqual(
    agentRequest(
      { kind: "codex", name: "@code-reviewer", persona: "  " },
      none,
    ),
    {
      kind: "ok",
      request: { kind: "codex", name: "code-reviewer", persona: null },
    },
  );
  assert.deepEqual(
    agentRequest(
      { kind: "terminal", name: " shell2 ", persona: " Runs tests " },
      none,
    ),
    {
      kind: "ok",
      request: { kind: "terminal", name: "shell2", persona: "Runs tests" },
    },
  );
});

test("add agent: names break the alias rule or collide with a member, and personas are bounded, each with the server's sentence", () => {
  const taken = takenNames([{ alias: "@codex" }, { alias: "@lead" }]);
  const draft = (name: string, persona = "") => ({
    kind: "claude" as const,
    name,
    persona,
  });
  for (const name of [
    "",
    "@",
    "@@x",
    "Code",
    "code reviewer",
    "-x",
    "x-",
    "a--b",
    "naïve",
  ])
    assert.deepEqual(
      agentRequest(draft(name), taken),
      problem(createCopy.agentNameRule),
      name,
    );
  assert.deepEqual(
    agentRequest(draft("@lead"), taken),
    problem("@lead is already in use in this channel. Pick a different name."),
  );
  assert.deepEqual(
    agentRequest(draft("helper", "x".repeat(1_001)), taken),
    problem(createCopy.personaLong),
  );
  assert.equal(
    agentRequest(draft("helper", "😀".repeat(1_000)), taken).kind,
    "ok",
    "the limit counts characters, not UTF-16 units",
  );
  for (const persona of ["line\nbreak", "tab\there", "rtl‮override"])
    assert.deepEqual(
      agentRequest(draft("helper", persona), taken),
      problem(createCopy.personaInvisible),
    );
});

test("add agent: the check while typing flags only what more typing can't fix", () => {
  const taken = takenNames([{ alias: "@codex" }]);
  for (const partial of ["", "@", "code", "code-", "@code-re"])
    assert.equal(liveNameProblem(partial, taken), null, partial);
  for (const wrong of ["Code", "code_x", "-code", "a--", "x y"])
    assert.equal(
      liveNameProblem(wrong, taken),
      createCopy.agentNameRule,
      wrong,
    );
  assert.equal(
    liveNameProblem("@codex", taken),
    "@codex is already in use in this channel. Pick a different name.",
  );
});

test("form: a press with a problem fails at once, a clean press waits, and only the wait takes the server's refusal", () => {
  const failed = formState(editing, { type: "submit", problem: "Bad." });
  assert.deepEqual(failed, { kind: "failed", message: "Bad." });
  assert.deepEqual(
    formState(editing, { type: "refused", message: "Late." }),
    editing,
    "a refusal with no press waiting changes nothing",
  );
  const pending = formState(failed, { type: "submit", problem: null });
  assert.deepEqual(pending, { kind: "pending" });
  assert.equal(
    formState(pending, { type: "submit", problem: null }),
    pending,
    "a second press while waiting sends nothing",
  );
  assert.equal(formState(pending, { type: "edit" }), pending);
  const refused = formState(pending, {
    type: "refused",
    message:
      "Herdr isn't reachable. Check that Herdr is running, then try again.",
  });
  assert.deepEqual(refused, {
    kind: "failed",
    message:
      "Herdr isn't reachable. Check that Herdr is running, then try again.",
  });
  const edited: FormState = formState(refused, { type: "edit" });
  assert.deepEqual(edited, editing, "editing clears the old refusal");
  assert.equal(formState(edited, { type: "edit" }), edited);
});
