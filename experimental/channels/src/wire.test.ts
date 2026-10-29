import assert from "node:assert/strict";
import { test } from "node:test";
import { terminalResult } from "./wire.ts";

const result = (text: string, truncated = false) =>
  terminalResult({ text, truncated });
const header = (code: string) =>
  `@shell command finished with exit code ${code}.\nstdout:\n`;

test("terminal results split stdout and stderr and drop the stdout newline", () => {
  const expected = { exitCode: 0, stdout: "hello", stderr: "" };
  assert.deepEqual(result(`${header("0")}hello\n\nstderr:\n`), expected);
  assert.deepEqual(result(`${header("0")}hello\n\nstderr:`), expected);
  assert.deepEqual(result(`${header("0")}a\n\nb\n\nstderr:\n`), {
    exitCode: 0,
    stdout: "a\n\nb",
    stderr: "",
  });
});
test("terminal results keep empty stdout empty", () => {
  const expected = { exitCode: 0, stdout: "", stderr: "" };
  assert.deepEqual(result(`${header("0")}\nstderr:\n`), expected);
  assert.deepEqual(result(`${header("0")}\nstderr:`), expected);
});
test("terminal results keep a non-empty stderr", () => {
  assert.deepEqual(result(`${header("127")}\nstderr:\nsh: nope: not found\n`), {
    exitCode: 127,
    stdout: "",
    stderr: "sh: nope: not found\n",
  });
  assert.deepEqual(result(`${header("2")}partial\n\nstderr:\nboom`), {
    exitCode: 2,
    stdout: "partial",
    stderr: "boom",
  });
});
test("terminal results split at the last stderr marker", () => {
  assert.deepEqual(
    result(`${header("0")}one\nstderr:\ngrep stderr: log\n\nstderr:\nwarn\n`),
    { exitCode: 0, stdout: "one\nstderr:\ngrep stderr: log", stderr: "warn\n" },
  );
  assert.deepEqual(result(`${header("0")}x\nstderr:\n\nstderr:`), {
    exitCode: 0,
    stdout: "x\nstderr:",
    stderr: "",
  });
});
test("terminal results read negative and large exit codes", () => {
  assert.equal(result(`${header("-1")}\nstderr:\n`)?.exitCode, -1);
  assert.equal(
    result(`${header("4294967295")}\nstderr:\n`)?.exitCode,
    4294967295,
  );
  assert.equal(result(`${header("99999999999999999999")}\nstderr:\n`), null);
});
test("truncated terminal results parse only after the stdout marker", () => {
  assert.equal(
    result("@shell command finished with exit code 0.\nstd", true),
    null,
  );
  assert.equal(result("@shell command finished with exit", true), null);
  assert.deepEqual(result(`${header("0")}line 1\nline 2`, true), {
    exitCode: 0,
    stdout: "line 1\nline 2",
    stderr: "",
  });
  assert.equal(result(`${header("0")}line 1\nline 2`), null);
});
test("other bodies are not terminal results", () => {
  assert.equal(result("please reply with the single word pong."), null);
  assert.equal(result("exit code 1"), null);
  assert.equal(result(`Note: ${header("0")}x\n\nstderr:\n`), null);
  assert.equal(
    result("@shell command finished with exit code 0.\nhello\n\nstderr:\n"),
    null,
  );
  assert.equal(
    result("@shell command finished with exit code zero.\nstdout:\n\nstderr:"),
    null,
  );
});
