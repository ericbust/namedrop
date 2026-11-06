import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  appendFile,
  chmod,
  lstat,
  mkdir,
  readFile,
  realpath,
  rm,
  writeFile,
} from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { createServer, request as httpsRequest } from "node:https";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { dnsName, initialState, ownerLogin } from "./tailscale.fake.mjs";

const execute = promisify(execFile);
const script = fileURLToPath(import.meta.url);
const checkout = fileURLToPath(new URL("../../../", import.meta.url));
const delay = (ms) => new Promise((done) => setTimeout(done, ms));
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;

if (process.argv[2] === "--fixture-terminal") {
  const receipt = process.argv[3];
  let count = 0;
  const timer = setInterval(() => {
    process.stdout.write(`\x1b[2J\x1b[HTAILSCALE-LIVE-${++count}\r\n`);
  }, 250);
  createInterface({ input: process.stdin }).on("line", (line) => {
    void appendFile(receipt, `${line}\n`);
    process.stdout.write(`\r\nINPUT-RECEIVED ${line}\r\n`);
  });
  process.on("SIGTERM", () => {
    clearInterval(timer);
    process.exit();
  });
} else if (process.argv.includes("--help")) {
  console.log(`Run against a dedicated testbed profile with no launched agents.

Prepare the profile through the testbed, which links logins read-only.
  TESTBED_HOME=/home/ebusta/.cache/tailscale/tb testbed/testbed up --link "$PWD" --profile test-tailscale --no-launch
Run the proof against the binary containing the Tailscale implementation.
  TESTBED_HOME=/home/ebusta/.cache/tailscale/tb node ${script} --profile test-tailscale --binary /absolute/path/to/namedrop

Options are --profile NAME, --binary PATH, --testbed-home DIR, and --output DIR.
TESTBED_HOME is required unless --testbed-home is supplied. CHROME overrides Chromium.
The proof uses fake Tailscale only, binds HTTPS to loopback port 8443, and refuses active agents.
It creates and closes one fixture pane. It leaves the testbed and evidence for inspection.
Evidence defaults to PROFILE/tailscale-proof/RUN. Output must stay inside the profile.
The profile must have no existing dashboard/tailscale.json configuration.`);
} else {
  await main();
}

async function main() {
  const options = {};
  for (let at = 2; at < process.argv.length; at += 2) {
    const name = process.argv[at];
    assert.ok(
      ["--profile", "--binary", "--testbed-home", "--output"].includes(name),
      `unknown option ${name}; use --help`,
    );
    assert.ok(process.argv[at + 1], `${name} needs a value`);
    options[name.slice(2)] = process.argv[at + 1];
  }
  assert.match(options.profile ?? "", /^test-[a-zA-Z0-9_-]+$/);
  const testbedHome = options["testbed-home"] ?? process.env.TESTBED_HOME;
  assert.ok(testbedHome, "set TESTBED_HOME or --testbed-home; use --help");
  const profile = resolve(testbedHome, "profiles", options.profile);
  assert.equal(
    await realpath(profile),
    profile,
    "profile must not be a symlink",
  );
  const marker = JSON.parse(await readFile(join(profile, ".namedrop-testbed")));
  assert.equal(marker.profile, profile);
  assert.match(marker.socket, /^\/tmp\/nd-tb-\d+-[a-f0-9]+\.sock$/);
  for (const name of ["home", "config", "state", "repo", "run", "bin"]) {
    assert.equal(await realpath(join(profile, name)), join(profile, name));
  }
  const saved = JSON.parse(await readFile(join(profile, "state.json")));
  assert.ok(saved.workspace && saved.panes.shell && saved.toolchain);
  const state = join(profile, "state/herdr/plugins/namedrop");
  assert.equal(await realpath(state), state);
  const binary = await realpath(
    options.binary ?? join(state, "runtime/namedrop"),
  );
  const output = resolve(
    options.output ?? join(profile, "tailscale-proof", randomUUID()),
  );
  assert.ok(
    output.startsWith(`${profile}/`),
    "evidence must stay inside the profile",
  );
  await mkdir(output, { recursive: true, mode: 0o700 });
  assert.equal(await realpath(output), output);
  const lock = join(profile, "tailscale-proof.lock");
  await mkdir(lock, { mode: 0o700 });
  const fakeDirectory = join(output, "fake-bin");
  await mkdir(fakeDirectory);
  const fakeFile = join(output, "fake-state.json");
  const commandLog = join(output, "tailscale-commands.jsonl");
  const receiptFile = join(output, "terminal-input.txt");
  const env = {
    PATH: `${fakeDirectory}:${join(profile, "bin")}:/usr/local/bin:/usr/bin:/bin`,
    HOME: join(profile, "home"),
    XDG_CONFIG_HOME: join(profile, "config"),
    XDG_STATE_HOME: join(profile, "state"),
    XDG_DATA_HOME: join(profile, "data"),
    XDG_CACHE_HOME: join(profile, "cache"),
    XDG_RUNTIME_DIR: join(profile, "run"),
    XDG_CONFIG_DIRS: join(profile, "config-dirs"),
    XDG_DATA_DIRS: join(profile, "data-dirs"),
    CODEX_HOME: join(profile, "codex"),
    HERDR_SOCKET_PATH: marker.socket,
    HERDR_CONFIG_PATH: join(profile, "config/herdr/config.toml"),
    RUSTUP_HOME: saved.toolchain.RUSTUP_HOME,
    CARGO_HOME: saved.toolchain.CARGO_HOME,
    TERM: "xterm-256color",
    LANG: "C.UTF-8",
    USER: String(process.getuid()),
    SHELL: "/bin/bash",
    TAILSCALE_PROOF_FAKE: "1",
    TAILSCALE_PROOF_STATE: fakeFile,
    TAILSCALE_PROOF_LOG: commandLog,
    NAMEDROP_DASHBOARD_IDLE_MS: "2000",
  };
  const alias = `tailscale-proof-${randomUUID().slice(0, 8)}`;
  const checks = [];
  const check = (name) => {
    checks.push(name);
    console.log(`PASS ${name}`);
  };
  const run = async (file, args, allowFailure = false) => {
    try {
      const result = await execute(file, args, {
        env,
        cwd: join(profile, "repo"),
        timeout: 30_000,
        maxBuffer: 4 * 1024 * 1024,
      });
      return { ...result, code: 0 };
    } catch (error) {
      if (allowFailure && typeof error.code === "number") return error;
      throw new Error(
        `${file} ${args.join(" ")} failed\n${error.stderr ?? error.message}`,
      );
    }
  };
  const namedrop = async (args, pane, allowFailure = false) => {
    for (let attempt = 0; attempt < 6; attempt++) {
      const result = await run(
        binary,
        [
          "--json",
          "--plugin-state",
          state,
          "--herdr-socket",
          marker.socket,
          "--herdr-workspace",
          saved.workspace,
          ...(pane ? ["--herdr-pane", pane] : []),
          ...args,
        ],
        true,
      );
      let value;
      try {
        value = JSON.parse(result.stdout);
      } catch {
        value = null;
      }
      if (JSON.stringify(value).includes("state_busy") && attempt < 5) {
        await delay(1500);
        continue;
      }
      if (!allowFailure) {
        assert.equal(result.code, 0, result.stderr || result.stdout);
        assert.ok(value, "Namedrop must return JSON");
      }
      return { ...result, value };
    }
  };
  const herdr = async (args) => {
    const result = await run(join(profile, "bin/herdr"), args);
    if (
      !result.stdout.trim() &&
      args[0] === "pane" &&
      ["run", "report-agent", "report-agent-session", "close"].includes(args[1])
    )
      return;
    const value = JSON.parse(result.stdout);
    assert.notEqual(value.ok, false, result.stdout);
    return value.result;
  };
  const readFake = async () => JSON.parse(await readFile(fakeFile));
  const writeFake = async (value) =>
    writeFile(fakeFile, JSON.stringify(value), { mode: 0o600 });
  let browser;
  let page;
  let proxy;
  let fixturePane;
  let installed = false;
  const errors = [];
  const proxyRequests = [];
  const liveRecords = [];
  let proxyLogin = ownerLogin;
  try {
    assert.equal(
      await lstat(join(state, "dashboard/tailscale.json")).catch(() => null),
      null,
      "use a fresh test- profile without existing Tailscale state",
    );
    const live = await herdr(["pane", "list", "--workspace", saved.workspace]);
    assert.ok(
      live.panes.every((pane) => !pane.agent),
      "the profile must have no launched agents",
    );
    await writeFile(
      join(fakeDirectory, "tailscale"),
      `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(fileURLToPath(new URL("./tailscale.fake.mjs", import.meta.url)))} "$@"\n`,
      { mode: 0o700 },
    );
    await chmod(join(fakeDirectory, "tailscale"), 0o700);
    const initial = initialState();
    const foreign = structuredClone(initial.serve);
    initial.status.BackendState = "Stopped";
    await writeFake(initial);
    const stopped = await namedrop(
      ["channels", "--no-open", "--tailscale"],
      null,
      true,
    );
    assert.notEqual(
      stopped.code,
      0,
      "a stopped Tailscale daemon must fail setup",
    );
    assert.deepEqual((await readFake()).serve, foreign);
    check("stopped daemon fails without changing Serve");
    initial.status.BackendState = "Running";
    await writeFake(initial);
    const enabled = await namedrop(["channels", "--no-open", "--tailscale"]);
    installed = true;
    const link = new URL(enabled.value.data.url);
    assert.equal(link.protocol, "https:");
    assert.equal(link.hostname, dnsName);
    assert.equal(link.port, "8443", "443 belongs to another Serve handler");
    assert.ok(
      link.searchParams.get("n"),
      "the HTTPS URL must contain a one-time sign-in key",
    );
    const record = JSON.parse(
      await readFile(join(state, "dashboard/tailscale.json")),
    );
    assert.equal(record.state, "enabled");
    assert.equal(record.handler.owner_login, ownerLogin);
    assert.equal(record.handler.dns_name, dnsName);
    assert.equal(record.handler.https_port, 8443);
    const backend = record.handler.backend_port;
    assert.equal(
      (await lstat(join(state, "dashboard/tailscale.json"))).mode & 0o777,
      0o600,
    );
    const configured = (await readFake()).serve;
    assert.deepEqual(
      configured.Web[`${dnsName}:443`],
      foreign.Web[`${dnsName}:443`],
    );
    assert.equal(
      configured.Web[link.host].Handlers["/"].Proxy,
      `http://127.0.0.1:${backend}`,
    );
    const again = await namedrop(["channels", "--no-open", "--tailscale"]);
    assert.equal(new URL(again.value.data.url).origin, link.origin);
    assert.deepEqual((await readFake()).serve, configured);
    check("enable preserves foreign 443, uses stable 8443, and is idempotent");
    const doctor = await namedrop(["doctor"], null, true);
    assert.ok(
      JSON.stringify(doctor.value).includes(link.origin),
      "Doctor reports the tailnet URL",
    );
    check("Doctor reports the configured HTTPS origin");
    await delay(3000);
    assert.equal(
      (
        await request(backend, "/api/session", {
          Host: `127.0.0.1:${backend}`,
        })
      ).status,
      401,
      "owned Serve backend must stay alive past idle without browser tabs",
    );
    check("owned Serve backend stays alive without tabs past the idle timeout");

    fixturePane = (
      await herdr([
        "pane",
        "split",
        saved.panes.shell,
        "--direction",
        "down",
        "--cwd",
        join(profile, "repo"),
        "--no-focus",
      ])
    ).pane.pane_id;
    await herdr([
      "pane",
      "run",
      fixturePane,
      `${quote(process.execPath)} ${quote(script)} --fixture-terminal ${quote(receiptFile)}`,
    ]);
    const session = randomUUID();
    await herdr([
      "pane",
      "report-agent",
      fixturePane,
      "--source",
      "tailscale-proof",
      "--agent",
      "codex",
      "--state",
      "idle",
      "--agent-session-id",
      session,
    ]);
    await herdr([
      "pane",
      "report-agent-session",
      fixturePane,
      "--source",
      "herdr:codex",
      "--agent",
      "codex",
      "--agent-session-id",
      session,
    ]);
    assert.equal(
      (
        await herdr(["pane", "list", "--workspace", saved.workspace])
      ).panes.find((pane) => pane.pane_id === fixturePane)?.agent_session
        ?.value,
      session,
      "the fixture must have a native Herdr session identity",
    );
    await namedrop(["name", `@${alias}`, "--type", "agent"], fixturePane);
    const fixture = await python(
      env,
      [state, marker.socket, saved.workspace, fixturePane, session],
      `
import json, sqlite3, sys, time, uuid
state, socket, workspace, pane, session = sys.argv[1:]
db = sqlite3.connect(state + '/namedrop.sqlite3', timeout=15)
db.execute('PRAGMA foreign_keys=ON')
row = db.execute('SELECT address_id,binding_id FROM pane_bindings WHERE session_socket_id=? AND worktree_id=? AND pane_id=? AND active=1', (socket, workspace, pane)).fetchone()
assert row, 'fixture pane must have a current Namedrop binding'
address, binding = row
db.execute('INSERT INTO agent_session_observations VALUES (?,?,?,?,?,?,?)', (socket,workspace,str(uuid.uuid4()),address,session,'idle',int(time.time()*1000)))
db.commit()
print(json.dumps({'address':address,'binding':binding}))
`,
    );

    const keyFile = join(output, "tls.key");
    const certFile = join(output, "tls.crt");
    await run("/usr/bin/openssl", [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-days",
      "1",
      "-subj",
      `/CN=${dnsName}`,
      "-addext",
      `subjectAltName=DNS:${dnsName}`,
      "-keyout",
      keyFile,
      "-out",
      certFile,
    ]);
    proxy = createServer(
      { key: await readFile(keyFile), cert: await readFile(certFile) },
      (req, res) => {
        const headers = Object.fromEntries(
          Object.entries(req.headers).filter(
            ([name]) => !/^(tailscale-user-|x-forwarded-)/i.test(name),
          ),
        );
        if (proxyLogin !== null) headers["tailscale-user-login"] = proxyLogin;
        headers["x-forwarded-host"] = req.headers.host;
        headers["x-forwarded-for"] = req.socket.remoteAddress;
        headers["x-forwarded-proto"] = "https";
        proxyRequests.push({
          method: req.method,
          path: req.url.split("?")[0],
          host: headers.host,
          origin: headers.origin ?? null,
          login: headers["tailscale-user-login"] ?? null,
        });
        const upstream = httpRequest(
          {
            hostname: "127.0.0.1",
            port: backend,
            path: req.url,
            method: req.method,
            headers,
          },
          (reply) => {
            res.writeHead(reply.statusCode, reply.headers);
            if (
              reply.statusCode === 200 &&
              req.url.split("?")[0].endsWith("/live")
            ) {
              let pending = "";
              reply.on("data", (bytes) => {
                pending += bytes.toString("utf8");
                const lines = pending.split("\n");
                pending = lines.pop();
                for (const line of lines) {
                  if (line.trim()) {
                    try {
                      liveRecords.push(JSON.parse(line));
                    } catch {
                      errors.push("Live Peek returned a malformed JSON record");
                    }
                  }
                }
              });
            }
            reply.pipe(res);
          },
        );
        upstream.on("error", (error) => res.destroy(error));
        res.on("close", () => upstream.destroy());
        req.pipe(upstream);
      },
    );
    await new Promise((done, reject) => {
      proxy.once("error", reject);
      proxy.listen(8443, "127.0.0.1", done);
    });
    const { chromium } = await import("playwright-core");
    browser = await chromium.launch({
      executablePath: process.env.CHROME ?? "/usr/bin/chromium",
      headless: true,
      env,
      args: [
        "--no-sandbox",
        "--no-proxy-server",
        `--host-resolver-rules=MAP ${dnsName} 127.0.0.1`,
      ],
    });
    const context = await browser.newContext({
      ignoreHTTPSErrors: true,
      viewport: { width: 1440, height: 1000 },
    });
    page = await context.newPage();
    page.setDefaultTimeout(20_000);
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(() => {
      window.tailnetEvents = [];
      const Native = window.EventSource;
      window.EventSource = class extends Native {
        constructor(...args) {
          super(...args);
          for (const type of ["space", "ready", "reset", "data"]) {
            this.addEventListener(type, (event) =>
              window.tailnetEvents.push({
                type,
                url: this.url,
                data: event.data,
              }),
            );
          }
          this.addEventListener("message", (event) =>
            window.tailnetEvents.push({
              type: "message",
              url: this.url,
              data: event.data,
            }),
          );
        }
      };
    });
    await page.goto(link.href);
    const member = page
      .getByRole("list", { name: "Members", exact: true })
      .getByRole("button")
      .filter({ hasText: `@${alias}` });
    await member.waitFor();
    const cookies = await context.cookies(link.origin);
    const cookie = cookies.find((item) =>
      item.name.startsWith("namedrop_dashboard_"),
    );
    assert.ok(
      cookie?.secure && cookie.httpOnly,
      "HTTPS session cookie is Secure and HttpOnly",
    );
    assert.equal(new URL(page.url()).searchParams.has("n"), false);
    check("Chromium signs in through HTTPS with a Secure session cookie");
    const key = enabled.value.data.space_key;
    const sessionReply = await page.evaluate(async () =>
      (await fetch("/api/session")).json(),
    );
    assert.ok(sessionReply.csrf_token);
    const cookieHeader = cookies
      .map((item) => `${item.name}=${item.value}`)
      .join("; ");
    const securityHeaders = {
      Host: link.host,
      Cookie: cookieHeader,
      Origin: link.origin,
      "Tailscale-User-Login": ownerLogin,
    };
    assert.equal(
      (await request(backend, "/api/session", securityHeaders)).status,
      200,
    );
    for (const [name, change] of [
      ["missing owner header", { "Tailscale-User-Login": undefined }],
      ["wrong owner", { "Tailscale-User-Login": "other@example.invalid" }],
      ["wrong origin", { Origin: "https://other.example.invalid" }],
      ["wrong host", { Host: "other.example.invalid:8443" }],
      ["cross-site request", { "Sec-Fetch-Site": "cross-site" }],
    ]) {
      assert.equal(
        (
          await request(backend, "/api/session", {
            ...securityHeaders,
            ...change,
          })
        ).status,
        403,
        name,
      );
      check(`rejects ${name}`);
    }
    const spoofed = await request(
      8443,
      "/api/session",
      {
        ...securityHeaders,
        "Tailscale-User-Login": "client-spoof@example.invalid",
        "Tailscale-User-Name": "spoof",
      },
      true,
    );
    assert.equal(
      spoofed.status,
      200,
      "proxy replaces a spoofed client login with its authenticated owner",
    );
    assert.equal(proxyRequests.at(-1).login, ownerLogin);
    proxyLogin = null;
    assert.equal(
      (await request(8443, "/api/session", securityHeaders, true)).status,
      403,
    );
    proxyLogin = "other@example.invalid";
    assert.equal(
      (await request(8443, "/api/session", securityHeaders, true)).status,
      403,
    );
    assert.equal(
      (
        await request(
          8443,
          "/api/session",
          {
            ...securityHeaders,
            Host: `127.0.0.1:${backend}`,
            Origin: undefined,
          },
          true,
        )
      ).status,
      403,
      "a foreign owner cannot bypass authentication with a loopback Host",
    );
    proxyLogin = null;
    assert.equal(
      (
        await request(
          8443,
          "/api/session",
          {
            ...securityHeaders,
            Host: `127.0.0.1:${backend}`,
            Origin: undefined,
          },
          true,
        )
      ).status,
      403,
      "a tagged or anonymous client cannot bypass authentication with a loopback Host",
    );
    proxyLogin = ownerLogin;
    check(
      "HTTPS proxy strips spoofed identity and rejects missing or foreign identity even with a loopback Host",
    );

    const postText = `@${alias} Tailnet public post ${randomUUID()}`;
    const channelComposer = page.getByRole("combobox", { name: /^Message #/ });
    await channelComposer.fill(postText);
    await channelComposer.press("Escape");
    const posting = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith(`/w/${key}/messages`),
    );
    await channelComposer.press("Enter");
    const postedResponse = await posting;
    assert.equal(postedResponse.status(), 200, await postedResponse.text());
    const posted = await postedResponse.json();
    const rootId = posted.message_id ?? posted.receipt?.message_id;
    assert.ok(rootId, JSON.stringify(posted));
    const row = page.locator(`[data-message-id="${rootId}"]`).first();
    await row.waitFor();
    await page.waitForFunction(
      (id) =>
        window.tailnetEvents.some(
          (event) =>
            event.url.endsWith("/api/events") && event.data.includes(id),
        ),
      rootId,
    );
    check("posting updates the real global SSE stream through the HTTPS proxy");
    await row.hover();
    await row.getByRole("button", { name: /reply in thread/i }).click();
    const thread = page.getByRole("complementary", {
      name: "Thread",
      exact: true,
    });
    await thread.waitFor();
    await thread
      .getByText(/Tailnet public post/)
      .first()
      .waitFor();
    check("thread opens through the proxy");

    const reminderId = `tailscale-${randomUUID()}`;
    await python(
      env,
      [
        state,
        marker.socket,
        saved.workspace,
        fixture.address,
        fixture.binding,
        fixturePane,
        session,
        rootId,
        reminderId,
      ],
      `
import json, sqlite3, sys, time
state, socket, workspace, address, binding, pane, session, message, reminder = sys.argv[1:]
db = sqlite3.connect(state + '/namedrop.sqlite3', timeout=15)
db.execute('PRAGMA foreign_keys=ON')
due = int(time.time()*1000) + 3600000
owner = {'address':address,'session':session,'binding':binding,'pane':pane}
spec = {'body':'Cancel the tailnet proof reminder','first_due_at_ms':due,'in_reply_to':message,'repeat':{'kind':'once'}}
phase = {'kind':'scheduled','due_at_ms':due,'ordinal':1}
db.execute('INSERT INTO reminders VALUES (?,?,?,?,?,?,?,?,?,?,?)', (socket,workspace,reminder,session,json.dumps(owner),json.dumps(spec),reminder,reminder,'scheduled',json.dumps(phase),due))
db.commit()
print('{}')
`,
    );
    await thread.locator(".conversation-reminders > summary").click();
    const cancelling = page.waitForResponse((response) =>
      response.url().endsWith(`/reminders/${reminderId}/cancel`),
    );
    await thread
      .getByRole("button", {
        name: "Cancel reminder Cancel the tailnet proof reminder",
        exact: true,
      })
      .click();
    assert.equal((await cancelling).status(), 200);
    await thread.getByText("No pending reminders.").waitFor();
    const cancelled = await python(
      env,
      [state, reminderId],
      `
import json, sqlite3, sys
db = sqlite3.connect(sys.argv[1] + '/namedrop.sqlite3')
print(json.dumps({'state':db.execute('SELECT state FROM reminders WHERE reminder_id=?',(sys.argv[2],)).fetchone()[0]}))
`,
    );
    assert.equal(cancelled.state, "cancelled");
    check(
      "reminder Cancel reaches the real scoped route and persists cancellation",
    );

    await member.click();
    await page
      .getByRole("region", { name: "Read-only live terminal", exact: true })
      .waitFor();
    await eventually(
      async () => liveRecords.some((record) => record.kind === "reset"),
      "Live Peek receives a reset through the proxy",
    );
    const liveBefore = liveRecords.length;
    await eventually(
      async () =>
        liveRecords.length > liveBefore &&
        liveRecords.some((record) => record.kind === "data"),
      "Live Peek receives another update before the response ends",
    );
    await page
      .getByRole("button", { name: "Type to agent", exact: true })
      .click();
    const inputText = `tailnet-input-${randomUUID()}`;
    const inputResponse = page.waitForResponse((response) =>
      response.url().endsWith(`/participants/${fixture.address}/input`),
    );
    await page
      .getByRole("combobox", {
        name: `Message to @${alias}`,
        exact: true,
      })
      .fill(inputText);
    await page
      .getByRole("combobox", {
        name: `Message to @${alias}`,
        exact: true,
      })
      .press("Enter");
    const inputReply = await inputResponse;
    if (inputReply.status() !== 204) {
      await writeFile(
        join(output, "input-diagnostic.json"),
        JSON.stringify(
          {
            native: await herdr([
              "pane",
              "list",
              "--workspace",
              saved.workspace,
            ]),
            fixture,
            session,
            responseStatus: inputReply.status(),
            frames: liveRecords.slice(-8),
            post: inputReply.request().postData(),
          },
          null,
          2,
        ),
      );
    }
    assert.equal(inputReply.status(), 204);
    await eventually(
      async () =>
        (await readFile(receiptFile, "utf8").catch(() => "")).includes(
          inputText,
        ),
      "fixture terminal receives browser input",
    );
    await page.screenshot({
      path: join(output, "https-live-peek.png"),
      fullPage: true,
    });
    check(
      "Live Peek continues streaming and Type to agent reaches the isolated terminal",
    );
    await page
      .getByRole("button", { name: "Close screen", exact: true })
      .click();

    await page.locator(`[data-dm-key="${key}/${fixture.address}"]`).click();
    const directText = `Tailnet private post ${randomUUID()}`;
    const directComposer = page.getByRole("combobox", {
      name: `Message @${alias} privately`,
      exact: true,
    });
    await directComposer.fill(directText);
    const sending = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith(`/direct-messages/${fixture.address}/messages`),
    );
    await directComposer.press("Enter");
    assert.equal((await sending).status(), 200);
    await page.getByText(directText, { exact: true }).waitFor();
    await page.screenshot({
      path: join(output, "https-direct-message.png"),
      fullPage: true,
    });
    check("direct messages post and reload through HTTPS");

    const local = await namedrop(["channels", "--no-open"]);
    const localLink = new URL(local.value.data.url);
    assert.equal(localLink.hostname, "127.0.0.1");
    assert.equal(Number(localLink.port), backend);
    const localSignIn = await request(
      backend,
      `${localLink.pathname}${localLink.search}`,
      { Host: localLink.host },
    );
    assert.ok([302, 303].includes(localSignIn.status));
    const localCookie = localSignIn.headers["set-cookie"]
      .map((value) => value.split(";")[0])
      .join("; ");
    assert.ok(
      localSignIn.headers["set-cookie"].every(
        (value) => !/;\s*Secure\b/i.test(value),
      ),
    );
    assert.equal(
      (
        await request(backend, "/api/session", {
          Host: localLink.host,
          Cookie: localCookie,
          Origin: localLink.origin,
        })
      ).status,
      200,
    );
    check("local sign-in and non-Secure localhost cookies remain usable");
    const wrongMutation = await request(
      backend,
      `/w/${key}/messages`,
      {
        ...securityHeaders,
        Origin: "https://evil.example.invalid",
        "X-Namedrop-Csrf": sessionReply.csrf_token,
        "Content-Type": "application/json",
      },
      false,
      JSON.stringify({
        request_id: randomUUID(),
        source: `@${alias} must not post`,
      }),
    );
    assert.equal(wrongMutation.status, 403);
    check(
      "wrong-origin mutation with a valid session and CSRF token is refused",
    );

    await namedrop(["channels", "--no-open", "--tailscale", "off"]);
    installed = false;
    assert.deepEqual((await readFake()).serve, foreign);
    assert.equal(
      (await request(backend, "/api/session", securityHeaders)).status,
      403,
    );
    assert.equal(
      (
        await request(
          8443,
          "/api/session",
          {
            ...securityHeaders,
            Host: `127.0.0.1:${backend}`,
            Origin: undefined,
          },
          true,
        )
      ).status,
      403,
      "proxy markers cannot bypass off using loopback Host",
    );
    assert.equal(
      (
        await request(backend, "/api/session", {
          Host: localLink.host,
          Cookie: localCookie,
          Origin: localLink.origin,
        })
      ).status,
      200,
    );
    await namedrop(["channels", "--no-open", "--tailscale", "off"]);
    assert.deepEqual((await readFake()).serve, foreign);
    check(
      "off revokes HTTPS trust, preserves foreign Serve handlers and local access, and is idempotent",
    );

    await namedrop(["channels", "--no-open", "--tailscale"]);
    installed = true;
    const replaced = await readFake();
    replaced.serve.Web[link.host].Handlers["/"].Proxy =
      "http://127.0.0.1:23456";
    await writeFake(replaced);
    const removeForeign = await namedrop(
      ["channels", "--no-open", "--tailscale", "off"],
      null,
      true,
    );
    assert.notEqual(
      removeForeign.code,
      0,
      "off must refuse a handler replaced by another application",
    );
    assert.deepEqual((await readFake()).serve, replaced.serve);
    check(
      "off refuses and preserves a handler replaced by another application",
    );
    replaced.serve.Web[link.host].Handlers["/"].Proxy =
      `http://127.0.0.1:${backend}`;
    await writeFake(replaced);
    await namedrop(["channels", "--no-open", "--tailscale", "off"]);
    installed = false;
    assert.deepEqual((await readFake()).serve, foreign);
    assert.deepEqual(errors, []);
    const commands = (await readFile(commandLog, "utf8"))
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.ok(
      commands.every((args) => args[0] === "status" || args[0] === "serve"),
    );
    assert.ok(
      commands
        .filter((args) => args.includes("--bg"))
        .every((args) => args.includes("--https=8443")),
    );
    assert.ok(proxyRequests.some((req) => req.path === "/api/events"));
    assert.ok(proxyRequests.some((req) => req.path.endsWith("/live")));
    assert.ok(
      proxyRequests
        .filter((req) => req.method === "POST")
        .every((req) => req.host === link.host && req.origin === link.origin),
    );
    check(
      "browser has no page errors and proxy preserves HTTPS Host and Origin on mutations",
    );
    await writeFile(
      join(output, "result.json"),
      JSON.stringify(
        {
          passed: true,
          checks,
          binary,
          profile,
          origin: link.origin,
          backend,
          errors,
          proxyRequests,
          liveRecords,
        },
        null,
        2,
      ),
    );
    console.log(`PASS Tailscale integration. Evidence ${output}`);
  } catch (error) {
    await page
      ?.screenshot({ path: join(output, "failure.png"), fullPage: true })
      .catch(() => {});
    await writeFile(
      join(output, "result.json"),
      JSON.stringify(
        {
          passed: false,
          checks,
          binary,
          profile,
          error: error.stack,
          errors,
          proxyRequests,
          liveRecords,
        },
        null,
        2,
      ),
    );
    console.error(`FAIL Tailscale integration. Evidence ${output}`);
    throw error;
  } finally {
    await browser?.close();
    if (proxy) {
      proxy.closeAllConnections();
      await new Promise((done) => proxy.close(done));
    }
    if (installed) {
      const result = await namedrop(
        ["channels", "--no-open", "--tailscale", "off"],
        null,
        true,
      );
      if (result.code !== 0)
        console.error(
          "Fake Serve cleanup failed. Inspect the isolated proof evidence and profile state.",
        );
    }
    if (fixturePane)
      await herdr(["pane", "close", fixturePane]).catch((error) =>
        console.error(error.message),
      );
    await rm(lock, { recursive: true });
  }
}

async function request(port, path, headers, tls = false, body) {
  return new Promise((done, reject) => {
    const client = (tls ? httpsRequest : httpRequest)(
      {
        hostname: "127.0.0.1",
        port,
        path,
        method: body === undefined ? "GET" : "POST",
        headers: Object.fromEntries(
          Object.entries({
            ...headers,
            ...(body === undefined
              ? {}
              : { "Content-Length": Buffer.byteLength(body) }),
          }).filter(([, value]) => value !== undefined),
        ),
        rejectUnauthorized: false,
        servername: tls ? dnsName : undefined,
      },
      (response) => {
        let raw = "";
        response.setEncoding("utf8");
        response.on("data", (chunk) => {
          raw += chunk;
        });
        response.on("end", () =>
          done({
            status: response.statusCode,
            headers: response.headers,
            body: raw,
          }),
        );
      },
    );
    client.on("error", reject);
    client.setTimeout(10_000, () =>
      client.destroy(new Error("HTTP proof request timed out")),
    );
    client.end(body);
  });
}

async function python(env, args, source) {
  const result = await execute("/usr/bin/python3", ["-c", source, ...args], {
    env,
    cwd: checkout,
    timeout: 20_000,
  });
  return JSON.parse(result.stdout);
}

async function eventually(predicate, message) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await delay(100);
  }
  assert.fail(message);
}
