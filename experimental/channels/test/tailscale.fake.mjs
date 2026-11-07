#!/usr/bin/env node
import assert from "node:assert/strict";
import { appendFile, readFile, rename, writeFile } from "node:fs/promises";

export const dnsName = "channels-proof.fixture.ts.net";
export const ownerLogin = "owner@example.invalid";

export function initialState() {
  return {
    status: {
      BackendState: "Running",
      Self: { DNSName: `${dnsName}.`, UserID: 42 },
      User: { 42: { ID: 42, LoginName: ownerLogin } },
    },
    serve: {
      TCP: { 443: { HTTPS: true } },
      Web: {
        [`${dnsName}:443`]: {
          Handlers: { "/": { Proxy: "http://127.0.0.1:12345" } },
        },
      },
      AllowFunnel: {},
    },
  };
}

export async function fakeTailscale(args, file, log) {
  assert.ok(file && log, "the fake requires its isolated state and log paths");
  await appendFile(log, `${JSON.stringify(args)}\n`);
  const state = JSON.parse(await readFile(file, "utf8"));
  if (args.join(" ") === "status --json") return state.status;
  if (args.join(" ") === "serve status --json") return state.serve;
  assert.equal(args[0], "serve", "only fake Serve operations are supported");
  const options = args.slice(1).filter((arg) => arg.startsWith("--"));
  assert.ok(options.includes("--bg"), "Serve must persist in the background");
  assert.ok(
    options.includes("--set-path=/"),
    "only the root handler is supported",
  );
  assert.ok(
    options.every((arg) => /^(--bg|--set-path=\/|--https=\d+)$/.test(arg)),
    "unsupported fake Serve option",
  );
  const port = options.find((arg) => arg.startsWith("--https="))?.slice(8);
  assert.ok(port && Number(port) >= 1 && Number(port) <= 65535);
  const positional = args.slice(1).filter((arg) => !arg.startsWith("--"));
  const authority = `${dnsName}:${port}`;
  const target = positional[0];
  assert.match(target ?? "", /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.ok(positional.length === 1 || positional[1] === "off");
  if (positional[1] === "off") {
    delete state.serve.Web[authority]?.Handlers["/"];
    if (!Object.keys(state.serve.Web[authority]?.Handlers ?? {}).length) {
      delete state.serve.Web[authority];
      delete state.serve.TCP[port];
    }
  } else {
    state.serve.TCP[port] = { HTTPS: true };
    state.serve.Web[authority] ??= { Handlers: {} };
    state.serve.Web[authority].Handlers["/"] = { Proxy: target };
  }
  await writeFile(`${file}.next`, JSON.stringify(state, null, 2), {
    mode: 0o600,
  });
  await rename(`${file}.next`, file);
  return null;
}

if (process.env.TAILSCALE_PROOF_FAKE === "1") {
  try {
    const value = await fakeTailscale(
      process.argv.slice(2),
      process.env.TAILSCALE_PROOF_STATE,
      process.env.TAILSCALE_PROOF_LOG,
    );
    if (value !== null) process.stdout.write(`${JSON.stringify(value)}\n`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
