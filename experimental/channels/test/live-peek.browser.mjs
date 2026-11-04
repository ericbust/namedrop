import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright-core";
import { isolatedFirefoxDisplay } from "./live-peek-scroll.browser.mjs";

// Decode the deliberately small static Rust literal used by the product.
// Unsupported syntax must stop the oracle, never select a remembered policy.
function securityHeaders(source) {
  const definitions = [
    ...source.matchAll(/^[ \t]*const\s+SECURITY_HEADERS\s*:/gm),
  ];
  assert.equal(
    definitions.length,
    1,
    "Expected one unambiguous SECURITY_HEADERS definition",
  );
  // Support the current imports/constants prologue, not arbitrary Rust lexing.
  // Refuse comment/raw-string/cfg ambiguity instead of reading an inactive example.
  const prologue = source
    .slice(0, definitions[0].index)
    .replace(/\/\/[^\n]*/g, "");
  assert.ok(
    !/\/\*|\*\/|\b(?:r|br|cr)#*"|\b(?:cfg|cfg_attr)\s*\(/.test(prologue),
    "Unsupported Rust prologue before SECURITY_HEADERS",
  );
  const unquotedPrologue = prologue.replace(/"(?:[^"\\]|\\[\s\S])*"/g, "");
  assert.ok(
    !/["']/.test(unquotedPrologue),
    "Ambiguous string/character literal before SECURITY_HEADERS",
  );
  const declaration = source.slice(definitions[0].index);
  const prefix = /^[ \t]*const\s+SECURITY_HEADERS\s*:\s*&str\s*=\s*"/.exec(
    declaration,
  );
  assert.ok(
    prefix,
    "SECURITY_HEADERS must be one ordinary static Rust string literal",
  );
  let at = prefix[0].length;
  let decoded = "";
  let terminated = false;
  while (at < declaration.length) {
    const char = declaration[at++];
    if (char === '"') {
      terminated = true;
      break;
    }
    if (char !== "\\") {
      decoded += char;
      continue;
    }
    const escaped = declaration[at++];
    if (escaped === "\n" || (escaped === "\r" && declaration[at] === "\n")) {
      if (escaped === "\r") at++;
      while (/[ \t\r\n]/.test(declaration[at] ?? "!")) at++;
      continue;
    }
    const escapes = { n: "\n", r: "\r", t: "\t", '"': '"', "\\": "\\" };
    assert.ok(
      Object.hasOwn(escapes, escaped),
      "Unsupported Rust escape in SECURITY_HEADERS",
    );
    decoded += escapes[escaped];
  }
  assert.ok(
    terminated && /^\s*;/.test(declaration.slice(at)),
    "SECURITY_HEADERS must end after that string literal",
  );
  assert.ok(
    decoded.endsWith("\r\n"),
    "SECURITY_HEADERS must contain complete CRLF header lines",
  );
  const headers = {};
  for (const line of decoded.slice(0, -2).split("\r\n")) {
    const field = /^([A-Za-z0-9-]+):[ \t]*([\x20-\x7e\t]+)$/.exec(line);
    assert.ok(field, "Malformed or unsupported SECURITY_HEADERS line");
    const name = field[1].toLowerCase();
    assert.ok(
      !Object.hasOwn(headers, name),
      "Duplicate SECURITY_HEADERS field",
    );
    headers[name] = field[2].trim();
  }
  const csp = headers["content-security-policy"];
  assert.ok(
    csp && !/[{}]/.test(csp),
    "Expected one static enforced CSP, without runtime placeholders",
  );
  return { headers, csp, decoded };
}

const securitySource = new URL(
  "../../../src/dashboard/http.rs",
  import.meta.url,
);
const securitySourceText = await readFile(securitySource, "utf8");
const productSecurity = securityHeaders(securitySourceText);
const csp = productSecurity.csp;
const securityEvidence = {
  source: fileURLToPath(securitySource),
  sourceSha256: createHash("sha256").update(securitySourceText).digest("hex"),
  headersSha256: createHash("sha256")
    .update(productSecurity.decoded)
    .digest("hex"),
};
const checkout = fileURLToPath(new URL("../../../", import.meta.url));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], {
  cwd: checkout,
  encoding: "utf8",
}).trim();
const sourceDirty =
  execFileSync("git", ["status", "--porcelain"], {
    cwd: checkout,
    encoding: "utf8",
  }).length !== 0;

test("CSP extraction tracks test-local policy changes in the authoritative Rust definition", () => {
  const marker = "oracle-local-marker; ";
  const changedSource = securitySourceText.replace(
    "Content-Security-Policy: ",
    `Content-Security-Policy: ${marker}`,
  );
  assert.notEqual(changedSource, securitySourceText);
  const changed = securityHeaders(changedSource);
  assert.equal(changed.csp, `${marker}${csp}`);
  assert.notEqual(
    changed.csp,
    csp,
    "A stale copied fixture policy must not satisfy this check",
  );
  const flatSource = `const SECURITY_HEADERS: &str = ${JSON.stringify(productSecurity.decoded)};`;
  assert.deepEqual(securityHeaders(flatSource), productSecurity);
});

test("CSP extraction fails closed on missing, ambiguous, malformed or unsupported source", () => {
  const literal = (value) =>
    `const SECURITY_HEADERS: &str = ${JSON.stringify(value)};`;
  for (const source of [
    "",
    securitySourceText.replace(
      "const SECURITY_HEADERS:",
      "const OTHER_HEADERS:",
    ),
    `${securitySourceText}\n${literal(productSecurity.decoded)}`,
    `/*\n${literal(productSecurity.decoded)}\n*/`,
    `// ${literal(productSecurity.decoded)}`,
    `const EXAMPLE: &str = r#"\n${literal(productSecurity.decoded)}\n"#;`,
    `#[cfg(feature = "disabled")]\n${literal(productSecurity.decoded)}`,
    'const SECURITY_HEADERS: &str = concat!("one", "two");',
    'const SECURITY_HEADERS: &str = r#"Content-Security-Policy: none"#;',
    'const SECURITY_HEADERS: &str = "unterminated',
    String.raw`const SECURITY_HEADERS: &str = "unsupported\u{61}";`,
    literal(
      productSecurity.decoded.replace(
        "Content-Security-Policy:",
        "Content-Security-Policy-Report-Only:",
      ),
    ),
    literal(`${productSecurity.decoded}Content-Security-Policy: ${csp}\r\n`),
    literal("Content-Security-Policy: {runtime_policy}\r\n"),
    literal("Content-Security-Policy: missing-crlf"),
  ])
    assert.throws(() => securityHeaders(source));
});

// Diagnostic runs cannot emit the gate's success token. The default oracle
// always uses the product CSP; this isolates style compatibility from other defects.
const styleDiagnostic = process.env.LIVE_PEEK_STYLE_DIAGNOSTIC === "1";
if (styleDiagnostic)
  assert.ok(
    /(?:^|;)\s*style-src 'self'(?=;|$)/.test(csp),
    "Diagnostic supports only the original self-only style directive",
  );
const servedCsp = styleDiagnostic
  ? csp.replace("style-src 'self'", "style-src 'self' 'unsafe-inline'")
  : csp;
const output =
  process.env.LIVE_PEEK_OUTPUT ??
  new URL("../../../.scratch/live-peek-r7/browser/", import.meta.url).pathname;
await mkdir(output, { recursive: true });
const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
};
const fixture = JSON.parse(
  (
    await readFile(
      new URL("../src/fixtures/real-frames.jsonl", import.meta.url),
      "utf8",
    )
  )
    .trim()
    .split("\n")[0],
);
fixture.frame.space = {
  ...fixture.frame.space,
  display_name: "alpha",
  addressable_label: "alpha",
  not_addressable_reason: null,
};
fixture.frame.participants = ["codex", "claude"].map((name) => ({
  ...fixture.frame.participants[0],
  address_id: name,
  alias: `@${name}`,
  activity: "working",
}));
const encode = (value) => Buffer.from(value).toString("base64");
const paint =
  "\x1b[2J\x1b[H\x1b[31mRED-SENTINEL\x1b[0m\x1b[2;4H\x1b[38;2;12;34;56mTRUECOLOR\x1b[0m\x1b[3;1HOLD-MARKER\x1b[5;7H\x1b[?25l";
const full = (seq = 1, extra = {}) => ({
  v: 1,
  seq,
  kind: "reset",
  continuity: "browser-continuity",
  cols: 40,
  rows: 10,
  bytes: encode(paint),
  ...extra,
});
const data = (seq, bytes) => ({
  v: 1,
  seq,
  kind: "data",
  bytes: encode(bytes),
});
const outcome = (reason, kind = "unavailable", seq = 1) => ({
  v: 1,
  seq,
  kind,
  reason,
});

function hostileText(origin, proof) {
  return [
    "HOSTILE-LITERAL",
    `</span><style id="peek-evil-style">html{opacity:.13;background-image:url("${origin}/hostile-css?proof=${proof}")}</style>`,
    '<script id="peek-evil-script">window.peekHostileExecuted=1</script>',
    `<img id="peek-evil-image" src="${origin}/hostile.png?proof=${proof}" onerror="window.peekHostileExecuted=1">`,
    `<link id="peek-evil-link" rel="stylesheet" href="${origin}/hostile.css?proof=${proof}">`,
    '<div id="peek-evil-node" style="position:fixed;inset:0;background:red">COVER-CONTROLS</div>',
    "javascript:window.peekHostileExecuted=1",
    "data:text/html,<script>window.peekHostileExecuted=1</script>",
    `CSS url("${origin}/hostile-url?proof=${proof}")`,
    "HOSTILE-END",
  ].join("\r\n");
}

function controlPolicy(policy, origin) {
  const additions = {
    "script-src": ` 'unsafe-inline' 'unsafe-eval' ${origin}`,
    "connect-src": ` ${origin}`,
    "style-src": ` ${origin}`,
    "img-src": ` ${origin}`,
  };
  return policy
    .split(";")
    .map((part) => {
      const name = part.trim().split(/\s+/)[0];
      return Object.hasOwn(additions, name) ? part + additions[name] : part;
    })
    .join(";");
}

function probeScript(origin, proof) {
  return `
  (() => {
    const state = window.cspProbe = { active: true, done: false, trustedExternal: 1,
      inlineScript: 0, eventHandler: 0, listener: 0, eval: 0, remoteScript: 0, violations: [] };
    document.addEventListener('securitypolicyviolation', e => state.violations.push({directive: e.effectiveDirective, blockedURI: e.blockedURI, disposition: e.disposition}));
    const root = document.createElement('div'); root.id = 'csp-probe-root'; root.hidden = true; document.body.append(root);
    const target = document.createElement('div'); target.id = 'csp-style-target'; root.append(target);
    state.styleBefore = getComputedStyle(target).color;
    const inline = document.createElement('script'); inline.textContent = 'window.cspProbe.inlineScript++'; root.append(inline);
    const button = document.createElement('button');
    button.addEventListener('click', () => state.listener++);
    button.setAttribute('onclick', 'window.cspProbe.eventHandler++'); root.append(button); button.click();
    try { (0, eval)('window.cspProbe.eval++'); state.evalError = null; } catch (error) { state.evalError = error.name; }
    const resource = (tag, path, configure) => new Promise(resolve => {
      const node = document.createElement(tag);
      node.onload = () => resolve(true); node.onerror = () => resolve(false);
      configure(node, ${JSON.stringify(origin)} + path + '?proof=' + ${JSON.stringify(proof)}); root.append(node);
    });
    Promise.all([
      resource('script', '/remote.js', (n, url) => n.src = url),
      resource('link', '/remote.css', (n, url) => { n.rel = 'stylesheet'; n.href = url; }),
      resource('img', '/remote.png', (n, url) => n.src = url),
      fetch(${JSON.stringify(origin)} + '/connect?proof=' + ${JSON.stringify(proof)}, {mode: 'no-cors'}).then(() => true, () => false),
    ]).then(async resources => {
      state.resources = resources;
      state.styleAfter = getComputedStyle(target).color;
      await new Promise(resolve => setTimeout(resolve, 100));
      state.active = false; state.done = true;
    });
  })();`;
}

async function open(browser, t, initial = full()) {
  const requests = [],
    streams = new Set(),
    allStreams = [];
  const browserRequests = [];
  const appAmbientRequests = [];
  const eventStreams = new Set();
  const frame = structuredClone(fixture);
  const probeRequests = [];
  const probeDetails = [];
  const ambientRequests = [];
  const proof = randomUUID();
  const probeServer = createServer((req, res) => {
    const url = new URL(req.url, "http://localhost");
    // Host port discovery can visit /. Only this run's tagged requests count
    // as browser probes; the router below rejects untagged browser traffic.
    if (url.searchParams.get("proof") !== proof) {
      ambientRequests.push({
        path: req.url,
        method: req.method,
        agent: req.headers["user-agent"],
      });
      return res.writeHead(404).end();
    }
    probeRequests.push(url.pathname);
    probeDetails.push({
      path: req.url,
      method: req.method,
      headers: req.headers,
    });
    const assets = {
      "/remote.js": ["text/javascript", "window.cspProbe.remoteScript++"],
      "/remote.css": [
        "text/css",
        "#csp-style-target{color:rgb(1,2,3)!important}",
      ],
      "/remote.png": [
        "image/png",
        Buffer.from(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a0uoAAAAASUVORK5CYII=",
          "base64",
        ),
      ],
      "/connect": ["text/plain", "CONTROL-CONNECT"],
    };
    const asset = assets[url.pathname];
    res.writeHead(asset ? 200 : 404, {
      "Content-Type": asset?.[0] ?? "text/plain",
      "Cache-Control": "no-store",
    });
    res.end(asset?.[1] ?? "Unexpected probe request");
  });
  await new Promise((resolve) => probeServer.listen(0, "127.0.0.1", resolve));
  const probeOrigin = `http://127.0.0.1:${probeServer.address().port}`;
  let mode = "normal",
    screens = 0;
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    if (req.headers["x-peek-fixture-proof"] !== proof) {
      appAmbientRequests.push({
        path: req.url,
        method: req.method,
        headers: req.headers,
      });
      return res.writeHead(404).end();
    }
    requests.push({
      path: url.pathname,
      query: url.search,
      method: req.method,
      cookie: req.headers.cookie,
    });
    if (url.pathname === "/csp-control") {
      res.writeHead(200, {
        "Content-Type": "text/html",
        ...productSecurity.headers,
        "content-security-policy": controlPolicy(csp, probeOrigin),
      });
      return res.end(
        '<!doctype html><title>CSP positive control</title><body><script src="/csp-probe.js"></script>',
      );
    }
    if (url.pathname === "/csp-probe.js") {
      res.writeHead(200, {
        "Content-Type": "text/javascript",
        "Cache-Control": "no-store",
      });
      return res.end(probeScript(probeOrigin, proof));
    }
    if (url.pathname === "/api/events") {
      eventStreams.add(res);
      res.on("close", () => eventStreams.delete(res));
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.write(
        `event: space\ndata: ${JSON.stringify(frame)}\n\nevent: ready\ndata: ${JSON.stringify({ keys: [frame.key] })}\n\n`,
      );
      return;
    }
    if (url.pathname === "/api/session") {
      res.writeHead(200, { "Content-Type": "application/json" });
      return res.end(JSON.stringify({ csrf_token: "fixture-csrf" }));
    }
    if (url.pathname.endsWith("/screen")) {
      screens++;
      if (mode === "session") return res.writeHead(401).end();
      if (mode === "missing") return res.writeHead(404).end();
      res.writeHead(200, { "Content-Type": "text/plain" });
      return res.end("SANITIZED TEXT SNAPSHOT");
    }
    if (url.pathname.endsWith("/live")) {
      assert.ok(req.headers.cookie?.includes("peek_fixture=ready"));
      if (mode === "session") return res.writeHead(401).end();
      if (mode === "replaced") return res.writeHead(410).end();
      res.writeHead(200, {
        "Content-Type": "application/x-ndjson",
        "Cache-Control": "no-store",
      });
      const stream = {
        res,
        member: url.pathname.split("/").at(-2),
        send(record) {
          res.write(`${JSON.stringify(record)}\n`);
        },
        ended: false,
      };
      streams.add(stream);
      allStreams.push(stream);
      res.on("close", () => {
        streams.delete(stream);
        stream.ended = true;
      });
      if (mode === "unsupported")
        return res.end(`${JSON.stringify(outcome("unsupported"))}\n`);
      if (mode === "invalid") return res.end(`${JSON.stringify(full(2))}\n`);
      if (mode === "delayed")
        setTimeout(() => {
          if (!res.destroyed) stream.send(initial);
        }, 250);
      else stream.send(initial);
      if (mode === "retry") res.end();
      return;
    }
    try {
      const isPage = url.pathname === "/" || url.pathname.startsWith("/w/");
      const path = isPage ? "/index.html" : url.pathname;
      const body = await readFile(new URL(`../dist${path}`, import.meta.url));
      res.writeHead(200, {
        "Content-Type": types[extname(path)] ?? "application/octet-stream",
        ...productSecurity.headers,
        "content-security-policy": servedCsp,
        ...(isPage
          ? {
              "Set-Cookie":
                "peek_fixture=ready; Path=/; HttpOnly; SameSite=Strict",
            }
          : {}),
      });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  let context;
  let closed = false;
  async function close() {
    if (closed) return;
    closed = true;
    try {
      await context?.close();
    } finally {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      probeServer.closeAllConnections();
      await new Promise((resolve) => probeServer.close(resolve));
    }
  }
  t.after(close);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    colorScheme: "dark",
  });
  const page = await context.newPage();
  page.setDefaultTimeout(5000);
  const errors = [],
    external = [],
    dialogs = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("dialog", (d) => {
    dialogs.push(d.type());
    void d.dismiss();
  });
  const origin = `http://127.0.0.1:${server.address().port}`;
  await context.route("**/*", (route) => {
    if (route.request().url().startsWith(`${origin}/`)) {
      browserRequests.push({
        path: new URL(route.request().url()).pathname,
        method: route.request().method(),
      });
      return route.continue({
        headers: {
          ...route.request().headers(),
          "x-peek-fixture-proof": proof,
        },
      });
    }
    if (route.request().url().startsWith(`${probeOrigin}/`)) {
      assert.equal(
        new URL(route.request().url()).searchParams.get("proof"),
        proof,
        "Untagged browser traffic to controlled origin",
      );
      return route.continue();
    }
    external.push(route.request().url());
    return route.abort();
  });
  await page.addInitScript((probeOrigin) => {
    window.peekPolicy = {
      violations: [],
      expectedProbes: [],
      clipboard: 0,
      opened: 0,
    };
    document.addEventListener("securitypolicyviolation", (e) => {
      const directive = e.effectiveDirective;
      const controlled =
        window.cspProbe?.active &&
        (((e.blockedURI === "inline" || e.blockedURI === "eval") &&
          directive.startsWith("script-src")) ||
          (e.blockedURI.startsWith(probeOrigin) &&
            /^(script-src|style-src|connect-src|img-src)/.test(directive)));
      if (controlled)
        window.peekPolicy.expectedProbes.push({
          directive,
          blockedURI: e.blockedURI,
          disposition: e.disposition,
        });
      else window.peekPolicy.violations.push(e.violatedDirective);
    });
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async () => {
          window.peekPolicy.clipboard++;
        },
        readText: async () => {
          window.peekPolicy.clipboard++;
          return "";
        },
      },
    });
    window.open = () => {
      window.peekPolicy.opened++;
      return null;
    };
  }, probeOrigin);
  const navigation = await page.goto(`${origin}/w/${fixture.key}`);
  assert.ok(navigation);
  assert.equal(
    navigation.headers()["content-security-policy"],
    servedCsp,
    "Fixture response must serve the extracted product policy",
  );
  await page.getByRole("heading", { name: "alpha", exact: true }).waitFor();
  await page.bringToFront();
  const chip = (member) =>
    page.locator(`[data-address-id="${member}"] .member`);
  const drawer = page.getByRole("complementary", { name: /^Screen of/ });
  const rows = () => drawer.locator(".xterm-rows > div");
  const live = () => allStreams.at(-1);
  const liveCount = () =>
    requests.filter((r) => r.path.endsWith("/live")).length;
  return {
    engine: browser.browserType().name(),
    page,
    context,
    drawer,
    chip,
    rows,
    live,
    liveCount,
    allStreams,
    streams,
    requests,
    browserRequests,
    appAmbientRequests,
    async requestControls() {
      const beforeBrowser = browserRequests.length;
      const beforeServer = requests.length;
      const ambient = await fetch(origin, {
        headers: { "user-agent": "namedrop-oracle-ambient-control" },
      });
      assert.equal(ambient.status, 404);
      await ambient.text();
      const status = await page.evaluate(async () => {
        const response = await fetch("/request-positive-control");
        await response.text();
        return response.status;
      });
      assert.equal(status, 404);
      assert.equal(browserRequests.length, beforeBrowser + 1);
      assert.equal(requests.length, beforeServer + 1);
      assert.equal(browserRequests.at(-1).path, "/request-positive-control");
      assert.equal(requests.at(-1).path, "/request-positive-control");
      assert.ok(
        appAmbientRequests.some(
          (r) => r.headers["user-agent"] === "namedrop-oracle-ambient-control",
        ),
      );
      return {
        browser: browserRequests.at(-1),
        received: requests.at(-1),
        ambientSeparated: true,
      };
    },
    errors,
    external,
    dialogs,
    probeRequests,
    probeDetails,
    hostile: hostileText(probeOrigin, proof),
    publishMessage(text) {
      frame.frame.messages = [
        {
          message_id: "hostile-message",
          thread_id: "hostile-message",
          in_reply_to: null,
          at_ms: 100,
          author: "codex",
          kind: "mention",
          body: { text, truncated: false },
          legs: [],
        },
      ];
      for (const response of eventStreams)
        response.write(`event: space\ndata: ${JSON.stringify(frame)}\n\n`);
    },
    async cspControls() {
      const control = await context.newPage();
      try {
        const response = await control.goto(`${origin}/csp-control`);
        assert.equal(
          response.headers()["content-security-policy"],
          controlPolicy(csp, probeOrigin),
        );
        await control.waitForFunction(() => window.cspProbe?.done);
        const positive = await control.evaluate(() => ({
          ...window.cspProbe,
          resourcesObserved: performance
            .getEntriesByType("resource")
            .map((e) => ({ name: e.name, initiator: e.initiatorType })),
        }));
        assert.deepEqual(
          [
            positive.trustedExternal,
            positive.inlineScript,
            positive.eventHandler,
            positive.listener,
            positive.eval,
            positive.remoteScript,
          ],
          [1, 1, 1, 1, 1, 1],
        );
        assert.deepEqual(positive.resources, [true, true, true, true]);
        assert.equal(positive.styleAfter, "rgb(1, 2, 3)");
        assert.deepEqual(positive.violations, []);
        assert.deepEqual([...probeRequests].sort(), [
          "/connect",
          "/remote.css",
          "/remote.js",
          "/remote.png",
        ]);
        const received = probeRequests.length;
        await page.evaluate(() => {
          const script = document.createElement("script");
          script.src = "/csp-probe.js";
          document.body.append(script);
        });
        await page.waitForFunction(() => window.cspProbe?.done);
        const negative = await page.evaluate(() => window.cspProbe);
        assert.deepEqual([negative.trustedExternal, negative.listener], [1, 1]);
        assert.deepEqual(
          [
            negative.inlineScript,
            negative.eventHandler,
            negative.eval,
            negative.remoteScript,
          ],
          [0, 0, 0, 0],
        );
        assert.equal(negative.evalError, "EvalError");
        assert.deepEqual(negative.resources, [false, false, false, false]);
        assert.equal(negative.styleAfter, negative.styleBefore);
        assert.equal(
          probeRequests.length,
          received,
          "CSP must prevent controlled-origin network traffic; the test router permits it",
        );
        const expected = await page.evaluate(
          () => window.peekPolicy.expectedProbes,
        );
        assert.ok(
          expected.filter(
            (e) =>
              e.blockedURI === "inline" && e.directive.startsWith("script-src"),
          ).length >= 2,
        );
        assert.ok(
          expected.some(
            (e) =>
              e.blockedURI === "eval" && e.directive.startsWith("script-src"),
          ),
        );
        for (const kind of [
          "script-src",
          "connect-src",
          "style-src",
          "img-src",
        ])
          assert.ok(
            expected.some(
              (e) =>
                e.blockedURI.startsWith(probeOrigin) &&
                e.directive.startsWith(kind),
            ),
            kind,
          );
        assert.deepEqual(
          await page.evaluate(() => window.peekPolicy.violations),
          [],
        );
        return {
          positive,
          negative,
          expectedViolations: expected,
          controlledOrigin: probeOrigin,
          receivedPositiveRequests: [...probeRequests],
          ambientRequests: [...ambientRequests],
          positivePolicy: controlPolicy(csp, probeOrigin),
        };
      } finally {
        await control.close();
      }
    },
    screens: () => screens,
    mode(value) {
      mode = value;
    },
    close,
  };
}
async function clickMember(ui, member) {
  // Headed Firefox can keep an animated, visible chip perpetually "unstable"
  // in Playwright after several contexts. Force only this setup click.
  await ui.chip(member).click(ui.engine === "firefox" ? { force: true } : {});
}
async function until(fn, message) {
  const deadline = Date.now() + 5000;
  while (!(await fn())) {
    if (Date.now() >= deadline) assert.fail(message);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

async function appearance(page) {
  return page.evaluate(() => {
    const html = getComputedStyle(document.documentElement);
    const body = getComputedStyle(document.body);
    const heading = getComputedStyle(document.querySelector("main h1"));
    return {
      opacity: html.opacity,
      background: html.backgroundImage,
      bodyColor: body.color,
      bodyFont: body.fontSize,
      headingColor: heading.color,
    };
  });
}

async function assertLiteralContent(ui, first) {
  const before = await appearance(ui.page);
  ui.publishMessage(ui.hostile);
  const article = ui.page.locator('article[data-message-id="hostile-message"]');
  await article.getByText("HOSTILE-LITERAL", { exact: false }).waitFor();
  await article.getByRole("button", { name: "Show more", exact: true }).click();
  const messageText = await article.textContent();
  for (const literal of [
    "<style",
    "<script",
    "<img",
    "<link",
    "javascript:",
    "data:text/html",
    "CSS url(",
    "HOSTILE-END",
  ])
    assert.ok(messageText.includes(literal), literal);
  assert.equal(
    await article.locator("style, script, img, link, iframe").count(),
    0,
  );
  first.send(
    full(2, {
      cols: 120,
      rows: 20,
      bytes: encode(`\x1b[2J\x1b[H${ui.hostile}\x1b[?25l`),
    }),
  );
  await ui.rows().getByText("HOSTILE-END", { exact: true }).waitFor();
  const terminalText = (await ui.rows().allTextContents()).join("\n");
  for (const literal of [
    "HOSTILE-LITERAL",
    "<style",
    "<script",
    "<img",
    "<link",
    "javascript:",
    "data:text/html",
    "CSS url(",
  ])
    assert.ok(terminalText.includes(literal), literal);
  assert.equal(
    await ui.page
      .locator(
        "#peek-evil-style, #peek-evil-script, #peek-evil-image, #peek-evil-link, #peek-evil-node",
      )
      .count(),
    0,
  );
  assert.equal(
    await ui.page.evaluate(() => window.peekHostileExecuted ?? 0),
    0,
  );
  assert.deepEqual(await appearance(ui.page), before);
  assert.deepEqual(ui.probeRequests, []);
  assert.deepEqual(ui.external, []);
  assert.deepEqual(
    await ui.page.evaluate(() => window.peekPolicy.violations),
    [],
  );
  first.send(full(3));
  await ui.rows().first().getByText("RED-SENTINEL", { exact: true }).waitFor();
}

function projection(rows = 60, label = "BOTTOM") {
  return full(1, {
    cols: 120,
    rows,
    bytes: encode(
      "\x1b[2J\x1b[H" +
        Array.from(
          { length: rows },
          (_, i) => `\x1b[${i + 1};1HROW-${i + 1}`,
        ).join("") +
        `\x1b[${rows - 1};100HRIGHT-EDGE\x1b[${rows};1H${label}\x1b[?25l`,
    ),
  });
}

async function scrollMetrics(ui) {
  return ui.page.locator(".peek-live").evaluate((el) => {
    const box = el.getBoundingClientRect();
    const marker = [...el.querySelectorAll(".xterm-rows > div")].at(-1);
    const rect = marker.getBoundingClientRect();
    return {
      top: el.scrollTop,
      left: el.scrollLeft,
      height: el.clientHeight,
      total: el.scrollHeight,
      width: el.clientWidth,
      totalWidth: el.scrollWidth,
      overflowX: getComputedStyle(el).overflowX,
      bottomVisible:
        rect.top >= box.top && rect.bottom <= box.top + el.clientHeight,
      bottomText: marker.textContent,
    };
  });
}

async function settleScroll(ui) {
  const settled = await ui.page.locator(".peek-live").evaluate(async (el) => {
    let top = el.scrollTop,
      left = el.scrollLeft,
      stable = 0;
    for (let frame = 0; frame < 120 && stable < 5; frame++) {
      await new Promise(requestAnimationFrame);
      stable = el.scrollTop === top && el.scrollLeft === left ? stable + 1 : 0;
      top = el.scrollTop;
      left = el.scrollLeft;
    }
    return stable >= 5;
  });
  assert.equal(
    settled,
    true,
    "Native scroll must reach a stable position within the bounded frame budget",
  );
}

function sparseProjection(rows = 70, latest = 35, label = "SPARSE-35") {
  return full(1, {
    cols: 120,
    rows,
    bytes: encode(
      "\x1b[2J\x1b[H" +
        Array.from(
          { length: latest },
          (_, i) => `\x1b[${i + 1};1HROW-${i + 1}`,
        ).join("") +
        `\x1b[${latest};1H\x1b[2K${label}\x1b[?25l`,
    ),
  });
}

async function usefulMetrics(ui, row, text = "") {
  return ui.page.locator(".peek-live").evaluate(
    (el, { row, text }) => {
      const box = el.getBoundingClientRect();
      const line = el.querySelectorAll(".xterm-rows > div")[row - 1];
      const rect = line.getBoundingClientRect();
      let marker = rect;
      if (text) {
        const nodes = [];
        const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
        while (walker.nextNode()) nodes.push(walker.currentNode);
        const source = nodes.map((node) => node.textContent).join("");
        const at = source.indexOf(text);
        if (at < 0) throw new Error(`Missing marker ${text}: ${source}`);
        const range = document.createRange();
        let offset = 0;
        for (const node of nodes) {
          const end = offset + node.textContent.length;
          if (at >= offset && at < end) range.setStart(node, at - offset);
          if (at + text.length > offset && at + text.length <= end)
            range.setEnd(node, at + text.length - offset);
          offset = end;
        }
        marker = range.getBoundingClientRect();
      }
      const padding = Number.parseFloat(getComputedStyle(el).paddingBottom);
      return {
        top: el.scrollTop,
        height: el.clientHeight,
        total: el.scrollHeight,
        usefulTop: Math.max(
          0,
          Math.min(
            el.scrollHeight - el.clientHeight,
            el.scrollTop + rect.bottom - box.top + padding - el.clientHeight,
          ),
        ),
        visible:
          marker.top >= box.top && marker.bottom <= box.top + el.clientHeight,
        markerTop: marker.top - box.top,
        markerBottom: marker.bottom - box.top,
      };
    },
    { row, text },
  );
}

async function activeNavigationOracle(browser, t, result) {
  const ui = await open(browser, t, projection(70, "IDLE-LATEST"));
  let timer;
  try {
    await ui.page.setViewportSize({ width: 900, height: 540 });
    await clickMember(ui, "codex");
    await ui.rows().last().getByText("IDLE-LATEST", { exact: true }).waitFor();
    const region = ui.page.locator(".peek-live");
    const top = async () => {
      await region.evaluate((el) => {
        el.scrollTop = 0;
      });
      await settleScroll(ui);
      await region.focus();
    };
    const reachedEnd = () =>
      ui.page.waitForFunction(
        () => {
          const el = document.querySelector(".peek-live");
          const row = el.querySelectorAll(".xterm-rows > div")[69];
          const box = el.getBoundingClientRect(),
            rect = row.getBoundingClientRect();
          return (
            el.scrollHeight - el.clientHeight - el.scrollTop <= 1 &&
            rect.top >= box.top &&
            rect.bottom <= box.top + el.clientHeight
          );
        },
        undefined,
        { timeout: 1500 },
      );
    await top();
    await ui.page.keyboard.press("Control+End");
    await reachedEnd();
    const idle = await usefulMetrics(ui, 70, "IDLE-LATEST");
    let seq = 2;
    const navigation = {};
    const beforeServer = ui.requests.length,
      beforeBrowser = ui.browserRequests.length;
    for (const key of ["Control+End", "PageDown"]) {
      await top();
      let updates = 0;
      const label = key === "Control+End" ? "END-FLOW" : "PAGE-FLOW";
      timer = setInterval(
        () =>
          ui.live().send(data(seq++, `\x1b[70;1H\x1b[2K${label}-${++updates}`)),
        20,
      );
      await ui.page.keyboard.press(key);
      let reached = true;
      try {
        if (key === "Control+End") await reachedEnd();
        else
          await ui.page.waitForFunction(
            () => {
              const el = document.querySelector(".peek-live");
              return el.scrollTop > el.clientHeight / 2;
            },
            undefined,
            { timeout: 1500 },
          );
        // Native animation must settle while data is still arriving.
        await settleScroll(ui);
      } catch {
        reached = false;
      }
      clearInterval(timer);
      timer = undefined;
      const sent = updates;
      assert.ok(sent > 0, "Navigation must overlap actual source writes");
      await ui
        .rows()
        .last()
        .getByText(`${label}-${sent}`, { exact: true })
        .waitFor();
      navigation[key] = {
        reached,
        updates: sent,
        metrics: await usefulMetrics(ui, 70, `${label}-${sent}`),
      };
    }
    result.activeNavigation = { idle, navigation };
    assert.deepEqual(
      Object.fromEntries(
        Object.entries(navigation).map(([key, value]) => [key, value.reached]),
      ),
      { "Control+End": true, PageDown: true },
      "Native keyboard navigation must finish while 20ms output continues",
    );

    await top();
    const box = await region.boundingBox();
    await ui.page.mouse.move(box.x + 80, box.y + box.height / 2);
    let updates = 0;
    timer = setInterval(
      () =>
        ui.live().send(data(seq++, `\x1b[70;1H\x1b[2KWHEEL-FLOW-${++updates}`)),
      20,
    );
    for (let step = 0; step < 12; step++) await ui.page.mouse.wheel(0, 7.5);
    await ui.page.waitForFunction(
      () => document.querySelector(".peek-live").scrollTop >= 70,
    );
    await settleScroll(ui);
    const held = (await scrollMetrics(ui)).top;
    const observed = updates;
    await until(
      () => updates >= observed + 5,
      "live updates continue after local wheel navigation",
    );
    clearInterval(timer);
    timer = undefined;
    await ui
      .rows()
      .last()
      .getByText(`WHEEL-FLOW-${updates}`, { exact: true })
      .waitFor();
    const wheel = await usefulMetrics(ui, 70, `WHEEL-FLOW-${updates}`);
    assert.ok(
      Math.abs(wheel.top - held) <= 1,
      "Continuing output preserves the wheel-selected reading position",
    );
    assert.deepEqual(ui.requests.slice(beforeServer), []);
    assert.deepEqual(ui.browserRequests.slice(beforeBrowser), []);
    Object.assign(result.activeNavigation, {
      wheel,
      updates,
      serverRequests: ui.requests.slice(beforeServer),
      browserRequests: ui.browserRequests.slice(beforeBrowser),
    });
    assert.deepEqual(ui.errors, []);
    assert.deepEqual(
      await ui.page.evaluate(() => window.peekPolicy.violations),
      [],
    );
    result.checks.push(
      "active output every20ms: idle End control, concurrent Ctrl+End/PageDown complete, fractional wheel reading position holds; zero browser/server control requests",
    );
  } finally {
    clearInterval(timer);
    await ui.close();
  }
}

async function sparseScrollingOracle(browser, t, result) {
  const ui = await open(browser, t, sparseProjection());
  try {
    await ui.page.setViewportSize({ width: 900, height: 540 });
    await clickMember(ui, "codex");
    const region = ui.page.locator(".peek-live");
    const painted = async (row, text) => {
      await ui
        .rows()
        .nth(row - 1)
        .getByText(text, { exact: true })
        .waitFor();
      await settleScroll(ui);
    };
    await painted(35, "SPARSE-35");
    const initial = await usefulMetrics(ui, 35, "SPARSE-35");
    await ui.page.screenshot({
      path: join(output, `${result.name}-sparse-initial.png`),
    });
    // Effective positive control: latest is in this projection and can be shown.
    await region.evaluate((el, top) => {
      el.scrollTop = top;
    }, initial.usefulTop);
    await settleScroll(ui);
    const control = await usefulMetrics(ui, 35, "SPARSE-35");
    assert.equal(control.visible, true);
    let seq = 2;
    const send = (bytes) => ui.live().send(data(seq++, bytes));
    const reset = (rows, latest, label, suffix = "") => {
      const record = sparseProjection(rows, latest, label);
      if (suffix)
        record.bytes = encode(
          Buffer.from(record.bytes, "base64").toString() + suffix,
        );
      ui.live().send({ ...record, seq: seq++ });
    };
    send("\x1b[50;1HADVANCE-50");
    await painted(50, "ADVANCE-50");
    const advanced = await usefulMetrics(ui, 50, "ADVANCE-50");
    result.sparse = { initial, control, advanced };
    assert.deepEqual(
      { initial: initial.visible, advanced: advanced.visible },
      { initial: true, advanced: true },
      "Sparse initial and fixed-grid advancing output must both stay visible",
    );

    const beforeInput = ui.requests.length;
    const beforeInputBrowser = ui.browserRequests.length;
    const bounds = await region.boundingBox();
    await ui.page.mouse.move(bounds.x + 80, bounds.y + bounds.height / 2);
    await ui.page.mouse.wheel(0, -240);
    await settleScroll(ui);
    const up = await usefulMetrics(ui, 50, "ADVANCE-50");
    send("\x1b[55;1HHELD-55");
    await painted(55, "HELD-55");
    const held = await usefulMetrics(ui, 55, "HELD-55");
    assert.ok(
      Math.abs(held.top - up.top) <= 1,
      "Sparse upscroll must hold under new output",
    );
    assert.equal(held.visible, false);
    await region.evaluate((el, top) => {
      el.scrollTop = top;
    }, held.usefulTop);
    await settleScroll(ui);
    send("\x1b[58;1HRESUMED-58");
    await painted(58, "RESUMED-58");
    assert.equal(
      (await usefulMetrics(ui, 58, "RESUMED-58")).visible,
      true,
      "Returning to useful latest resumes following before the physical grid end",
    );

    reset(70, 50, "RETAINED-50", "\x1b[5;1H");
    await painted(50, "RETAINED-50");
    const cursorAbove = await usefulMetrics(ui, 50, "RETAINED-50");
    assert.equal(
      cursorAbove.visible,
      true,
      "A cursor above retained text cannot hide the latest retained row",
    );
    reset(70, 35, "TRAILING-35", "\x1b[69;1H   \x1b[35;1H");
    await painted(35, "TRAILING-35");
    assert.equal(
      (await usefulMetrics(ui, 35, "TRAILING-35")).visible,
      true,
      "Trailing spaces do not turn the blank grid tail into useful output",
    );
    send("\x1b[50;1H");
    await settleScroll(ui);
    const cursorBelow = await usefulMetrics(ui, 50);
    assert.equal(
      cursorBelow.visible,
      true,
      "The cursor row remains reachable below retained text",
    );
    assert.ok(Math.abs(cursorBelow.top - cursorBelow.usefulTop) <= 1);

    // Active alternate-buffer content must not inherit the normal-buffer extent.
    send("\x1b[?1049h\x1b[22;1HALTERNATE-22");
    await painted(22, "ALTERNATE-22");
    const alternate = await usefulMetrics(ui, 22, "ALTERNATE-22");
    assert.equal(alternate.visible, true);
    assert.ok(Math.abs(alternate.top - alternate.usefulTop) <= 1);
    send("\x1b[?1049l");
    await painted(35, "TRAILING-35");
    assert.equal((await usefulMetrics(ui, 50)).visible, true);
    send("\x1b[2J\x1b[H");
    await ui.page.waitForFunction(() =>
      [...document.querySelectorAll(".xterm-rows > div")].every(
        (row) => row.textContent.trim() === "",
      ),
    );
    await settleScroll(ui);
    assert.equal(
      (await usefulMetrics(ui, 1)).top,
      0,
      "Cleared grid with home cursor starts at the top",
    );
    reset(80, 0, "");
    await ui.page.waitForFunction(() => {
      const rows = [...document.querySelectorAll(".xterm-rows > div")];
      return (
        rows.length === 80 && rows.every((row) => row.textContent.trim() === "")
      );
    });
    await settleScroll(ui);
    assert.equal(
      (await usefulMetrics(ui, 1)).top,
      0,
      "An empty full reset starts at the home cursor",
    );
    send("\x1b[40;1HAFTER-CLEAR-40");
    await painted(40, "AFTER-CLEAR-40");
    assert.equal((await usefulMetrics(ui, 40, "AFTER-CLEAR-40")).visible, true);
    reset(90, 45, "TALL-GRID-45");
    await painted(45, "TALL-GRID-45");
    assert.equal((await usefulMetrics(ui, 45, "TALL-GRID-45")).visible, true);
    await ui.page.setViewportSize({ width: 900, height: 420 });
    await settleScroll(ui);
    assert.equal((await usefulMetrics(ui, 45, "TALL-GRID-45")).visible, true);
    reset(20, 10, "SHORT-GRID-10");
    await painted(10, "SHORT-GRID-10");
    assert.equal((await usefulMetrics(ui, 10, "SHORT-GRID-10")).top, 0);
    reset(70, 35, "RESET-35");
    await painted(35, "RESET-35");
    assert.equal((await usefulMetrics(ui, 35, "RESET-35")).visible, true);
    assert.deepEqual(ui.browserRequests.slice(beforeInputBrowser), []);
    assert.deepEqual(ui.requests.slice(beforeInput), []);

    const count = ui.liveCount();
    ui.live().res.end();
    await until(() => ui.liveCount() > count, "sparse reconnect");
    await painted(35, "SPARSE-35");
    assert.equal((await usefulMetrics(ui, 35, "SPARSE-35")).visible, true);
    const box = await region.boundingBox();
    await ui.page.mouse.move(box.x + 80, box.y + box.height / 2);
    await ui.page.mouse.wheel(0, -200);
    await settleScroll(ui);
    const reconnectHeld = await usefulMetrics(ui, 35, "SPARSE-35");
    const old = ui.liveCount();
    ui.live().res.end();
    await until(() => ui.liveCount() > old, "sparse held reconnect");
    await painted(35, "SPARSE-35");
    assert.ok(
      Math.abs(
        (await usefulMetrics(ui, 35, "SPARSE-35")).top - reconnectHeld.top,
      ) <= 1,
    );
    ui.live().send(data(999, "INVALID-SEQUENCE"));
    await ui.drawer
      .getByRole("button", { name: "Try again", exact: true })
      .click();
    await painted(35, "SPARSE-35");
    assert.equal(
      (await usefulMetrics(ui, 35, "SPARSE-35")).visible,
      true,
      "Explicit retry starts at useful latest",
    );
    Object.assign(result.sparse, {
      up,
      held,
      cursorAbove,
      cursorBelow,
      alternate,
      reconnectHeld,
    });
    assert.equal(ui.screens(), 0);
    assert.deepEqual(ui.errors, []);
    assert.deepEqual(
      await ui.page.evaluate(() => window.peekPolicy.violations),
      [],
    );
    result.checks.push(
      "sparse useful latest: 70/35 first paint, advance to 50, hold/resume before blank tail; retained text above cursor, trailing spaces, cursor below text, active alternate buffer, clear/empty, grid/window resize, reconnect/retry",
    );
  } catch (error) {
    await ui.page.screenshot({
      path: join(output, `${result.name}-sparse-failure.png`),
    });
    throw error;
  } finally {
    await ui.close();
  }
}

async function scrollingOracle(browser, t, result) {
  const ui = await open(browser, t, projection());
  try {
    await ui.page.setViewportSize({ width: 1024, height: 620 });
    await clickMember(ui, "codex");
    await ui.rows().last().getByText("BOTTOM", { exact: true }).waitFor();
    const region = ui.page.locator(".peek-live");
    await ui.page.waitForTimeout(150);
    const initial = await scrollMetrics(ui);
    await ui.page.screenshot({
      path: join(output, `${result.name}-scroll-initial.png`),
    });
    const bounds = await region.boundingBox();
    await ui.page.mouse.move(bounds.x + 80, bounds.y + bounds.height / 2);
    await ui.page.mouse.wheel(0, -240);
    await ui.page.waitForTimeout(150);
    const wheel = await scrollMetrics(ui);
    // A pixel-delta input is the browser path used by a precision trackpad.
    const cancellation = await region
      .locator(".xterm-screen")
      .evaluate((el) => {
        const event = new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: -37.5,
          deltaMode: WheelEvent.DOM_DELTA_PIXEL,
        });
        el.dispatchEvent(event);
        return { prevented: event.defaultPrevented };
      });
    await ui.page.mouse.wheel(0, -37.5);
    await ui.page.waitForTimeout(150);
    const trackpad = await scrollMetrics(ui);
    result.scrolling = { initial, wheel, trackpad, cancellation };
    await ui.page.screenshot({
      path: join(output, `${result.name}-scroll-up.png`),
    });
    assert.ok(
      initial.total > initial.height * 1.5,
      "Real oversized projection is required",
    );
    assert.ok(
      initial.totalWidth > initial.width * 1.5,
      "Real narrow projection is required",
    );
    assert.ok(
      initial.bottomVisible,
      "First completed paint must show latest/bottom output",
    );
    assert.ok(
      wheel.top < initial.top - 100,
      "Actual browser wheel must move the local viewport up",
    );
    assert.ok(
      trackpad.top < wheel.top - 10,
      "Fractional pixel trackpad input must actually scroll",
    );
    assert.equal(
      cancellation.prevented,
      false,
      "Pixel trackpad must reach the native outer scroller",
    );
    assert.equal(
      initial.overflowX,
      "auto",
      "Oversized columns need an accessible horizontal scrollbar",
    );
    result.requestControls = await ui.requestControls();
    const beforeInput = ui.requests.length;
    const beforeInputBrowser = ui.browserRequests.length;
    let seq = 2;
    const send = (bytes) => ui.live().send(data(seq++, bytes));
    const reset = (rows, label) =>
      ui.live().send({ ...projection(rows, label), seq: seq++ });
    const atBottom = () =>
      ui.page.waitForFunction(() => {
        const el = document.querySelector(".peek-live");
        return el.scrollHeight - el.clientHeight - el.scrollTop <= 1;
      });
    const painted = async (text) => {
      await ui.rows().last().getByText(text, { exact: true }).waitFor();
      // Include subsequent render/scroll/ResizeObserver deliveries.
      await ui.page.waitForTimeout(100);
      assert.equal((await ui.rows().last().textContent()).trim(), text);
    };
    send("\x1b[60;1H\x1b[2KUPSCROLL-UPDATE");
    await painted("UPSCROLL-UPDATE");
    const held = await scrollMetrics(ui);
    assert.ok(
      Math.abs(held.top - trackpad.top) <= 1,
      "New data must not drag an upscrolled reader down",
    );
    assert.equal(held.bottomVisible, false);
    reset(60, "RESET-HELD");
    await painted("RESET-HELD");
    const resetHeld = await scrollMetrics(ui);
    assert.ok(
      Math.abs(resetHeld.top - held.top) <= 1,
      "Full reset preserves the local reading position",
    );
    assert.equal(resetHeld.bottomVisible, false);

    await ui.page.setViewportSize({ width: 1024, height: 540 });
    await settleScroll(ui);
    assert.ok(
      Math.abs((await scrollMetrics(ui)).top - resetHeld.top) <= 1,
      "Window resize retains intentional upscroll",
    );
    await ui.page.setViewportSize({ width: 1024, height: 620 });
    await settleScroll(ui);
    assert.ok(Math.abs((await scrollMetrics(ui)).top - resetHeld.top) <= 1);
    await ui.page.mouse.wheel(0, 3000);
    await atBottom();
    send("\x1b[60;1H\x1b[2KFOLLOW-AGAIN");
    await painted("FOLLOW-AGAIN");
    assert.equal((await scrollMetrics(ui)).bottomVisible, true);
    reset(80, "TALLER-RESET");
    await painted("TALLER-RESET");
    assert.equal(
      (await scrollMetrics(ui)).bottomVisible,
      true,
      "Following survives a taller full reset",
    );

    // Exercise both axes over the terminal, including alternate screen + mouse tracking.
    send("\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[80;1H\x1b[2KMOUSE-ALT");
    await painted("MOUSE-ALT");
    const mouseStart = await scrollMetrics(ui);
    await ui.page.mouse.wheel(0, -160);
    await ui.page.waitForTimeout(150);
    const mouseUp = await scrollMetrics(ui);
    assert.ok(
      mouseUp.top < mouseStart.top - 80,
      "Source mouse/alternate-screen modes cannot consume local wheel input",
    );
    await ui.page.mouse.wheel(2000, 0);
    await ui.page.waitForFunction(
      () => document.querySelector(".peek-live").scrollLeft > 100,
    );
    await settleScroll(ui);
    await ui.page.mouse.wheel(2000, 0);
    await settleScroll(ui);
    const horizontal = await scrollMetrics(ui);
    send("\x1b[79;100HRIGHT-EDGE\x1b[80;1H\x1b[2KEDGE-READY");
    await painted("EDGE-READY");
    await ui.page.mouse.wheel(0, 4000);
    await atBottom();
    await settleScroll(ui);
    const edge = await ui
      .rows()
      .nth(78)
      .evaluate((row) => {
        const el = row.closest(".peek-live");
        const box = el.getBoundingClientRect();
        const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
        while (walker.nextNode()) {
          const node = walker.currentNode;
          const at = node.textContent.indexOf("RIGHT-EDGE");
          if (at < 0) continue;
          const range = document.createRange();
          range.setStart(node, at);
          range.setEnd(node, at + "RIGHT-EDGE".length);
          const rect = range.getBoundingClientRect();
          return (
            rect.left >= box.left && rect.right <= box.left + el.clientWidth
          );
        }
        return false;
      });
    Object.assign(result.scrolling, {
      horizontal,
      edgeMetrics: await scrollMetrics(ui),
      edge,
    });
    assert.equal(edge, true, "Far-right source output must be reachable");
    await region.focus();
    await ui.page.keyboard.press("PageUp");
    await ui.page.waitForFunction(
      (top) => document.querySelector(".peek-live").scrollTop < top - 50,
      (await scrollMetrics(ui)).total - (await scrollMetrics(ui)).height,
    );
    await settleScroll(ui);
    const keyUp = await scrollMetrics(ui);
    send("\x1b[80;1H\x1b[2KKEY-HELD");
    await painted("KEY-HELD");
    assert.ok(Math.abs((await scrollMetrics(ui)).top - keyUp.top) <= 1);
    await ui.page.keyboard.press("Control+Home");
    await ui.page.waitForFunction(
      () => document.querySelector(".peek-live").scrollTop === 0,
    );
    await settleScroll(ui);
    // Drag the actual native vertical scrollbar thumb, not an injected scrollTop.
    const bar = await region.boundingBox();
    const metrics = await scrollMetrics(ui);
    const thumb = (metrics.height * metrics.height) / metrics.total;
    await ui.page.mouse.move(bar.x + bar.width - 3, bar.y + thumb / 2);
    await ui.page.mouse.down();
    await ui.page.mouse.move(
      bar.x + bar.width - 3,
      bar.y + metrics.height / 2,
      { steps: 12 },
    );
    await ui.page.mouse.up();
    await settleScroll(ui);
    const scrollbar = await scrollMetrics(ui);
    assert.ok(
      scrollbar.top > 100,
      "Dragging the native scrollbar must move the local viewport",
    );
    await region.focus();
    await ui.page.keyboard.press("Control+End");
    await atBottom();
    await settleScroll(ui);
    for (const size of [
      { width: 1024, height: 420 },
      { width: 1024, height: 760 },
    ]) {
      await ui.page.setViewportSize(size);
      await atBottom();
      assert.equal(
        (await scrollMetrics(ui)).bottomVisible,
        true,
        "Window resize keeps bottom followers at latest",
      );
    }
    // A source resize changes only the projected grid, never the source via this view.
    reset(35, "SOURCE-RESIZE");
    await painted("SOURCE-RESIZE");
    assert.equal((await scrollMetrics(ui)).bottomVisible, true);
    reset(80, "FOLLOW-RESET");
    await painted("FOLLOW-RESET");
    assert.equal((await scrollMetrics(ui)).bottomVisible, true);
    assert.deepEqual(ui.browserRequests.slice(beforeInputBrowser), []);
    assert.deepEqual(
      ui.requests.slice(beforeInput),
      [],
      "Local wheel/trackpad/scrollbar/keys/resizes must emit zero host requests",
    );

    result.scrolling.localRequests = ui.requests.slice(beforeInput);
    result.scrolling.browserRequests =
      ui.browserRequests.slice(beforeInputBrowser);
    result.scrolling.ambientAppRequests = ui.appAmbientRequests;
    const count = ui.liveCount();
    ui.live().res.end();
    await until(() => ui.liveCount() > count, "followed reconnect");
    await painted("BOTTOM");
    seq = 2;
    assert.equal(
      (await scrollMetrics(ui)).bottomVisible,
      true,
      "Reconnect followers stay at bottom",
    );
    const reconnectBounds = await region.boundingBox();
    await ui.page.mouse.move(
      reconnectBounds.x + 80,
      reconnectBounds.y + reconnectBounds.height / 2,
    );
    await ui.page.mouse.wheel(0, -200);
    await ui.page.waitForTimeout(150);
    const reconnectHeld = await scrollMetrics(ui);
    const countHeld = ui.liveCount();
    ui.live().res.end();
    await until(() => ui.liveCount() > countHeld, "upscrolled reconnect");
    await painted("BOTTOM");
    const reconnected = await scrollMetrics(ui);
    assert.ok(
      Math.abs(reconnected.top - reconnectHeld.top) <= 1,
      "Reconnect preserves intentional upscroll",
    );
    await ui.chip("claude").click();
    await until(
      () => ui.live()?.member === "claude",
      "scroll oracle target switch",
    );
    await painted("BOTTOM");
    assert.equal(
      (await scrollMetrics(ui)).bottomVisible,
      true,
      "Fresh target starts latest",
    );
    assert.ok(
      ui.allStreams
        .filter((stream) => stream.member === "codex")
        .every((stream) => stream.ended),
    );
    await ui.page.keyboard.press("Escape");
    await ui.drawer.waitFor({ state: "detached" });
    await until(
      () => ui.streams.size === 0,
      "scroll oracle close aborts streams",
    );
    await ui.chip("claude").click();
    await painted("BOTTOM");
    assert.equal(
      (await scrollMetrics(ui)).bottomVisible,
      true,
      "Reopening starts latest",
    );
    assert.equal(ui.screens(), 0);
    assert.deepEqual(ui.errors, []);
    assert.deepEqual(ui.external, []);
    assert.deepEqual(
      await ui.page.evaluate(() => window.peekPolicy.violations),
      [],
    );
    Object.assign(result.scrolling, {
      held,
      resetHeld,
      mouseUp,
      horizontal,
      scrollbar,
      reconnectHeld,
      reconnected,
    });
    result.checks.push(
      "oversized projection: initial latest; native wheel/fractional pixel/scrollbar/keyboard and horizontal reach; upscroll holds across writes/reset/reconnect; bottom resumes across resets/resizes/reconnect; fresh target/reopen latest; zero input/resize/focus/query transport",
    );
  } catch (error) {
    result.scrollFailure = await scrollMetrics(ui).catch(() => null);
    await ui.page.screenshot({
      path: join(output, `${result.name}-scroll-failure.png`),
    });
    throw error;
  } finally {
    await ui.close();
  }
}

async function historyAndFitOracle(browser, t, result) {
  const initial = full(1, {
    cols: 58,
    rows: 30,
    bytes: encode("\x1b[2J\x1b[HINITIAL-LIVE\x1b[30;1HEND-LIVE"),
  });
  const ui = await open(browser, t, initial);
  try {
    await ui.page.bringToFront();
    await clickMember(ui, "codex");
    await until(() => ui.live(), "history oracle live stream");
    let seq = 2;
    const history = (start) =>
      Array.from(
        { length: 2100 },
        (_, index) => `HISTORY-${String(start + index).padStart(4, "0")}`,
      ).join("\n");
    ui.live().send({ v: 1, seq: seq++, kind: "history", text: history(1) });
    await ui.page.locator(".peek-history span").nth(2099).waitFor();
    let historyStart = 1;
    for (const size of [
      { width: 1280, height: 800 },
      { width: 2560, height: 1440 },
    ]) {
      await ui.page.bringToFront();
      await ui.page.setViewportSize(size);
      await ui.page.waitForTimeout(100);
      const fit = await ui.page.locator(".peek-live").evaluate((el) => {
        const screen = el.querySelector(".xterm-screen");
        const style = getComputedStyle(el);
        const available =
          el.clientWidth -
          parseFloat(style.paddingLeft) -
          parseFloat(style.paddingRight);
        return {
          screen: screen.getBoundingClientRect().width,
          available,
          font: parseFloat(
            getComputedStyle(el.querySelector(".peek-history pre")).fontSize,
          ),
        };
      });
      assert.ok(
        fit.screen <= fit.available + 2,
        `Terminal should fit at ${size.width}px: ${JSON.stringify(fit)}`,
      );
      assert.ok(fit.font >= 10, "Terminal text stays readable");
      assert.equal(await ui.page.locator(".peek-history span").count(), 2100);
      assert.equal(
        await ui.page
          .locator(".peek-region-label")
          .allTextContents()
          .then((labels) => labels.join(" / ")),
        "Recent output / Live screen",
      );
      await ui.page
        .locator(".peek-history span")
        .nth(999)
        .evaluate((line) => {
          const scroller = line.closest(".peek-live");
          scroller.scrollTop +=
            line.getBoundingClientRect().top -
            scroller.getBoundingClientRect().top;
        });
      await settleScroll(ui);
      const reading = await ui.page
        .locator(".peek-history span")
        .nth(999)
        .evaluate(
          (line) =>
            line.getBoundingClientRect().top -
            line.closest(".peek-live").getBoundingClientRect().top,
        );
      historyStart += 100;
      ui.live().send({
        v: 1,
        seq: seq++,
        kind: "history",
        text: history(historyStart),
      });
      ui.live().send(data(seq++, "\x1b[30;1H\x1b[2KDATA-LIVE"));
      ui.live().send({
        ...initial,
        seq: seq++,
        bytes: encode("\x1b[2J\x1b[HRESET-LIVE"),
      });
      await ui.page.waitForTimeout(300);
      const held = await ui.page
        .locator(".peek-history span")
        .nth(899)
        .evaluate(
          (line) =>
            line.getBoundingClientRect().top -
            line.closest(".peek-live").getBoundingClientRect().top,
        );
      assert.ok(
        Math.abs(held - reading) < 3,
        `History reading anchor moved at ${size.width}px: ${held} vs ${reading}`,
      );
      assert.equal(
        await ui.page.locator(".peek-history span").count(),
        2100,
        "Snapshot replacement must not accumulate duplicates",
      );
      await ui.page.screenshot({
        path: join(output, `${result.name}-history-${size.width}-reading.png`),
      });
      await ui.page.locator(".peek-live").evaluate((el) => {
        el.scrollTop = el.scrollHeight;
      });
      await settleScroll(ui);
      await ui
        .rows()
        .first()
        .getByText("RESET-LIVE", { exact: true })
        .waitFor();
      ui.live().send(data(seq++, "\x1b[30;1H\x1b[2KFOLLOW-LIVE"));
      await ui
        .rows()
        .last()
        .getByText("FOLLOW-LIVE", { exact: true })
        .waitFor();
      const bottom = await scrollMetrics(ui);
      assert.ok(
        bottom.bottomVisible,
        "Returning to bottom follows the live screen",
      );
      await ui.page.screenshot({
        path: join(
          output,
          `${result.name}-history-${size.width}-following.png`,
        ),
      });
    }
    const expand = ui.drawer.getByRole("button", { name: "Expand" });
    await expand.click();
    assert.equal(
      await ui.drawer
        .getByRole("button", { name: "Restore" })
        .getAttribute("aria-pressed"),
      "true",
    );
    await ui.drawer.getByRole("button", { name: "Restore" }).click();
    assert.deepEqual(ui.errors, []);
    assert.deepEqual(
      await ui.page.evaluate(() => window.peekPolicy.violations),
      [],
    );
    result.checks.push(
      "2100 recent lines, bounded rotation anchor, fit and follow at 1280x800 and 2560x1440; expand/restore",
    );
  } finally {
    await ui.close();
  }
}

async function watchSnapshotInvalidationOracle(browser, t, result) {
  const row = (index) =>
    `Cache identity output ${String(index).padStart(4, "0")} with unique evidence`;
  const paintGrid = (start) =>
    Array.from(
      { length: 20 },
      (_, i) => `\x1b[${i + 1};1H\x1b[2K${row(start + i)}`,
    ).join("");
  const ui = await open(
    browser,
    t,
    full(1, { cols: 96, rows: 20, bytes: encode(`\x1b[2J${paintGrid(0)}`) }),
  );
  try {
    const capture = async (member) => {
      await clickMember(ui, member);
      await ui.page.locator(".peek-watching").waitFor();
      ui.live().send(data(2, paintGrid(5)));
      await until(
        async () => (await ui.page.locator(".peek-history span").count()) === 5,
        `${member} cache capture`,
      );
    };
    await capture("codex");
    await ui.drawer.getByRole("button", { name: "Close screen" }).click();
    await capture("claude");
    ui.mode("session");
    await ui.drawer
      .getByRole("button", { name: "Text snapshot", exact: true })
      .click();
    await ui.drawer.getByText("Session ended", { exact: true }).waitFor();
    ui.mode("normal");
    await ui.drawer.getByRole("button", { name: "Close screen" }).click();
    await clickMember(ui, "codex");
    await ui.page.locator(".peek-watching").waitFor();
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      0,
      "session refusal learned through snapshot mode clears every reader-scoped pane",
    );
    assert.equal(
      ui.requests.filter((request) => request.path.endsWith("/live")).at(-1)
        .query,
      "",
    );
    ui.live().send(data(2, paintGrid(5)));
    await until(
      async () => (await ui.page.locator(".peek-history span").count()) === 5,
      "fresh history after session replacement",
    );
    ui.mode("missing");
    await ui.drawer
      .getByRole("button", { name: "Text snapshot", exact: true })
      .click();
    await ui.drawer.getByText(/^Not in .+ any more$/).waitFor();
    ui.mode("normal");
    await ui.drawer.getByRole("button", { name: "Live", exact: true }).click();
    await ui.page.locator(".peek-watching").waitFor();
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      0,
      "missing target learned through snapshot mode invalidates retained watch history",
    );
    assert.equal(
      ui.requests.filter((request) => request.path.endsWith("/live")).at(-1)
        .query,
      "",
    );
    assert.deepEqual(ui.errors, []);
    result.checks.push(
      "snapshot session refusal clears all reader-scoped watch caches; snapshot missing target invalidates that pane; later live requests carry no stale continuity",
    );
  } finally {
    await ui.close();
  }
}

async function recordedWatchOracle(browser, t, result) {
  const recordings = JSON.parse(
    await readFile(
      new URL(
        "../src/fixtures/watch-history/recorded-cases.json",
        import.meta.url,
      ),
      "utf8",
    ),
  );
  for (const scenario of recordings.cases) {
    const first = scenario.updates[0].grid;
    const paintGrid = (grid) =>
      grid.rows.map((text, y) => `\x1b[${y + 1};1H\x1b[2K${text}`).join("");
    const resetGrid = (grid, seq) =>
      full(seq, {
        cols: grid.cols,
        rows: grid.rows.length,
        bytes: encode(`\x1b[2J${paintGrid(grid)}`),
      });
    const ui = await open(browser, t, resetGrid(first, 1));
    try {
      await clickMember(ui, "codex");
      await until(() => ui.live(), `${scenario.name} recorded stream`);
      await ui.page.locator(".peek-watching").waitFor();
      let seq = 2;
      for (const update of scenario.updates.slice(1)) {
        ui.live().send(
          update.kind === "reset"
            ? resetGrid(update.grid, seq++)
            : data(seq++, paintGrid(update.grid)),
        );
      }
      const last = scenario.updates.at(-1).grid;
      let observedRows = [];
      try {
        await until(async () => {
          observedRows = await ui.rows().allTextContents();
          return (
            observedRows.length === last.rows.length &&
            observedRows.every((text, i) => text.trimEnd() === last.rows[i])
          );
        }, `${scenario.name} completed product xterm grid`);
      } catch (error) {
        const differences = observedRows.flatMap((text, i) =>
          text.trimEnd() === last.rows[i]
            ? []
            : [{ row: i, observed: text, expected: last.rows[i] }],
        );
        throw new Error(
          `${scenario.name} product grid differs: ${JSON.stringify(differences.slice(0, 3))}`,
          { cause: error },
        );
      }
      assert.deepEqual(
        await ui.page.locator(".peek-history span").allTextContents(),
        scenario.expectedDeparted,
        `${scenario.name} uses independently selected departed rows`,
      );
      assert.equal(ui.screens(), 0);
      assert.deepEqual(ui.errors, []);
      result.checks.push(
        `recorded grids ${scenario.name}, native ${scenario.nativeSeq.join(" to ")}, expected ${scenario.expectedDeparted.length} departed rows`,
      );
    } finally {
      await ui.close();
    }
  }
}

async function shellSeedWatchOracle(browser, t, result) {
  const row = (index) =>
    `SHELL-SEED-${String(index).padStart(4, "0")} unique plain output evidence`;
  const grid = (start) => Array.from({ length: 40 }, (_, i) => row(start + i));
  const paintGrid = (rows) =>
    rows.map((text, y) => `\x1b[${y + 1};1H\x1b[2K${text}`).join("");
  const ui = await open(
    browser,
    t,
    full(1, {
      cols: 96,
      rows: 40,
      bytes: encode(`\x1b[2J${paintGrid(grid(20))}`),
    }),
  );
  try {
    await clickMember(ui, "codex");
    await until(() => ui.live(), "shell seed stream");
    await ui.page.locator(".peek-watching").waitFor();
    ui.live().send({
      v: 1,
      seq: 2,
      kind: "history",
      text: Array.from({ length: 26 }, (_, i) => row(i)).join("\n"),
    });
    await until(
      async () => (await ui.page.locator(".peek-history span").count()) === 20,
      "seed/live sequence overlap",
    );
    ui.live().send(data(3, paintGrid(grid(25))));
    await until(
      async () => (await ui.page.locator(".peek-history span").count()) === 25,
      "shell watched append",
    );
    ui.live().send({
      v: 1,
      seq: 4,
      kind: "history",
      text: Array.from({ length: 31 }, (_, i) => row(i)).join("\n"),
    });
    ui.live().send({
      ...full(5),
      cols: 96,
      rows: 40,
      bytes: encode(`\x1b[2J${paintGrid(grid(25))}`),
    });
    await ui.rows().first().getByText(row(25), { exact: true }).waitFor();
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      Array.from({ length: 25 }, (_, i) => row(i)),
      "shell seed and watched contribution never duplicate",
    );
    assert.equal(ui.screens(), 0);
    assert.deepEqual(ui.errors, []);
    result.checks.push(
      "passive shell seed/live exact overlap and later seed replacement preserve watched rows without duplicates",
    );
  } finally {
    await ui.close();
  }
}

async function watchedHistoryOracle(browser, t, result) {
  const row = (index) =>
    `  Watched output ${String(index).padStart(5, "0")} has unique scrolling evidence`;
  const grid = (start) => [
    "PINNED WATCH HEADER",
    ...Array.from({ length: 254 }, (_, i) => row(start + i)),
    "PINNED WATCH FOOTER",
  ];
  const paintGrid = (rows) =>
    rows.map((text, y) => `\x1b[${y + 1};1H\x1b[2K${text}`).join("");
  const initial = full(1, {
    cols: 96,
    rows: 256,
    bytes: encode(`\x1b[2J${paintGrid(grid(0))}`),
  });
  const ui = await open(browser, t, initial);
  try {
    await clickMember(ui, "codex");
    await until(() => ui.live(), "watched history stream");
    await ui.page
      .locator(".peek-watching")
      .filter({ hasText: /^Watching since \d/ })
      .waitFor();
    assert.equal(await ui.page.locator(".peek-history span").count(), 0);
    const since = await ui.page.locator(".peek-watching").textContent();
    let seq = 2;
    const sendGrid = async (start) => {
      ui.live().send(data(seq++, paintGrid(grid(start))));
      await until(
        async () =>
          (await ui.page.locator(".peek-history span").last().textContent()) ===
          row(start - 1),
        "completed watched frame",
      );
    };
    await sendGrid(100);
    await ui.page.locator(".peek-history span").nth(99).waitFor();
    assert.deepEqual(
      await ui.page.locator(".peek-history span").allTextContents(),
      Array.from({ length: 100 }, (_, i) => row(i)),
    );
    assert.equal(await ui.rows().first().textContent(), "PINNED WATCH HEADER");
    assert.equal(
      (await ui.rows().last().textContent()).trimEnd(),
      "PINNED WATCH FOOTER",
    );
    ui.live().send({
      ...initial,
      seq: seq++,
      bytes: encode(`\x1b[2J${paintGrid(grid(500))}`),
    });
    await ui
      .rows()
      .nth(1)
      .getByText(row(500).trim(), { exact: true })
      .waitFor();
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      100,
      "reset establishes a baseline without invented rows",
    );
    await sendGrid(600);
    assert.equal(await ui.page.locator(".peek-history span").count(), 200);
    ui.live().send(data(seq++, `\x1b[2J${paintGrid(grid(700))}`));
    await ui
      .rows()
      .nth(1)
      .getByText(row(700).trim(), { exact: true })
      .waitFor();
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      200,
      "clear and repaint cannot add rows",
    );
    const old = ui.live();
    await ui.drawer.getByRole("button", { name: "Close screen" }).click();
    await until(() => old.ended, "watch closes observation");
    ui.mode("delayed");
    await clickMember(ui, "codex");
    await until(
      () => ui.liveCount() === 2,
      "reopen identity validation request",
    );
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      0,
      "cached rows stay hidden before continuity validation",
    );
    assert.equal(await ui.page.locator(".peek-watching").isVisible(), false);
    await ui.page
      .locator(".peek-watching")
      .filter({ hasText: /^Watching since/ })
      .waitFor();
    assert.equal(await ui.page.locator(".peek-watching").textContent(), since);
    assert.equal(
      await ui.page.locator(".peek-history span").count(),
      200,
      "validated reopen retains rows but never infers the closed interval",
    );
    assert.ok(
      ui.requests
        .filter((request) => request.path.endsWith("/live"))
        .at(-1)
        .query.includes("continuity=browser-continuity"),
    );
    seq = 2;
    for (let start = 100; start <= 3000; start += 100) await sendGrid(start);
    await ui.page.locator(".peek-history span").nth(3199).waitFor();
    const anchor = ui.page.locator(".peek-history span").nth(1700);
    const id = await anchor.getAttribute("data-row-id");
    await anchor.evaluate((line) => {
      const scroller = line.closest(".peek-live");
      scroller.scrollTop +=
        line.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
    });
    await settleScroll(ui);
    const anchorOffset = () =>
      ui.page
        .locator(`.peek-history span[data-row-id="${id}"]`)
        .evaluate(
          (line) =>
            line.getBoundingClientRect().top -
            line.closest(".peek-live").getBoundingClientRect().top,
        );
    const before = await anchorOffset();
    for (let start = 3100; start <= 5200; start += 100) await sendGrid(start);
    assert.equal(await ui.page.locator(".peek-history span").count(), 5000);
    assert.ok(
      Math.abs((await anchorOffset()) - before) < 3,
      "surviving occurrence stays anchored through append and eviction",
    );
    let flowingStart = 5200;
    let flowingUpdates = 0;
    let producing = true;
    const producer = (async () => {
      while (producing) {
        await sendGrid(++flowingStart);
        flowingUpdates++;
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    })();
    let reached = true;
    try {
      await until(
        () => flowingUpdates > 0,
        "confirmed append before native return",
      );
      await ui.page.locator(".peek-live").focus();
      const beforeNavigation = flowingUpdates;
      await ui.page.keyboard.press("Control+End");
      try {
        await ui.page.waitForFunction(
          () => {
            const element = document.querySelector(".peek-live");
            return (
              element.scrollHeight - element.scrollTop - element.clientHeight <=
              1
            );
          },
          undefined,
          { timeout: 1500 },
        );
        await settleScroll(ui);
      } catch {
        reached = false;
      }
      result.watchedNavigation = {
        reached,
        confirmedAppendsDuringNavigation: flowingUpdates - beforeNavigation,
        metrics: await scrollMetrics(ui),
      };
      assert.ok(
        flowingUpdates > beforeNavigation,
        "Control+End overlaps actual confirmed watched appends",
      );
      assert.ok(
        reached,
        "Native Control+End finishes while confirmed history appends continue",
      );
    } finally {
      producing = false;
      await producer;
    }
    await sendGrid(flowingStart + 1);
    assert.ok(
      (await scrollMetrics(ui)).bottomVisible,
      "Control+End resumes following subsequent confirmed watched rows",
    );
    await ui.page.screenshot({
      path: join(output, `${result.name}-watch-following.png`),
    });
    ui.mode("replaced");
    const retainedStream = ui.live();
    await ui.drawer.getByRole("button", { name: "Close screen" }).click();
    await until(
      () => retainedStream.ended,
      "retained stream closes before identity check",
    );
    await clickMember(ui, "codex");
    await ui.drawer
      .getByText(
        "This pane changed. Reopen the member to see its current screen.",
        { exact: true },
      )
      .waitFor();
    assert.equal(await ui.page.locator(".peek-history span").count(), 0);
    assert.equal(await ui.page.locator(".peek-watching").isVisible(), false);
    assert.equal(ui.screens(), 0);
    assert.deepEqual(ui.errors, []);
    result.checks.push(
      "watched middle-band rows, fixed header/footer, local time marker, reset/clear negatives, stable reading through 5000-line eviction, native Control+End during confirmed appends, resumed following, validated reopen and replacement invalidation; no snapshot reads",
    );
  } finally {
    await ui.close();
  }
}

test("live Peek product component, transport and CSP in real Chromium and Firefox", {
  timeout: 180000,
}, async (t) => {
  const results = [];
  let failed = false;
  for (const [name, type, executablePath] of [
    ["chromium", chromium, process.env.CHROME ?? "/usr/bin/chromium"],
    ["firefox", firefox, process.env.FIREFOX ?? firefox.executablePath()],
  ]) {
    const browser = await type.launch({
      executablePath,
      headless: true,
      ignoreDefaultArgs: ["--hide-scrollbars"],
      ...(name === "firefox" ? await isolatedFirefoxDisplay(t, output) : {}),
      ...(name === "chromium" ? { args: ["--no-sandbox"] } : {}),
    });
    const result = {
      name,
      scope: styleDiagnostic
        ? "NON-GATING relaxed-style diagnostic"
        : "product CSP",
      csp: servedCsp,
      securitySource: securityEvidence,
      securityHeaders: productSecurity.headers,
      sourceCommit,
      sourceDirty,
      diagnostic: styleDiagnostic,
      version: browser.version(),
      executablePath,
      checks: [],
      status: "running",
    };
    results.push(result);
    let ui;
    try {
      await recordedWatchOracle(browser, t, result);
      await watchedHistoryOracle(browser, t, result);
      await shellSeedWatchOracle(browser, t, result);
      await watchSnapshotInvalidationOracle(browser, t, result);
      await activeNavigationOracle(browser, t, result);
      await sparseScrollingOracle(browser, t, result);
      await scrollingOracle(browser, t, result);
      await historyAndFitOracle(browser, t, result);
      ui = await open(browser, t);
      await clickMember(ui, "codex");
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      assert.equal(ui.screens(), 0);
      assert.equal(
        await ui.drawer
          .getByRole("button", { name: "Live", exact: true })
          .getAttribute("aria-pressed"),
        "true",
      );
      assert.equal(await ui.rows().nth(2).textContent(), "OLD-MARKER");
      assert.deepEqual(
        await ui.page.evaluate(() => window.peekPolicy.violations),
        [],
        "Real product CSP must admit the renderer without violations",
      );
      const colors = await ui
        .rows()
        .evaluateAll((rows) => [
          getComputedStyle(rows[0].querySelector("span")).color,
          getComputedStyle(
            [...rows[1].querySelectorAll("span")].find((s) =>
              s.textContent.includes("TRUECOLOR"),
            ),
          ).color,
        ]);
      assert.equal(colors[1], "rgb(12, 34, 56)");
      assert.equal(colors[0], "rgb(204, 0, 0)");
      assert.ok(
        (await ui.drawer
          .locator(".xterm style")
          .evaluateAll(
            (styles) =>
              styles.filter((s) => s.sheet?.cssRules.length > 0).length,
          )) >= 2,
        "Trusted generated renderer CSS must actually be applied",
      );
      result.checks.push(
        styleDiagnostic
          ? "diagnostic initial paint/colors with relaxed style policy; no snapshot request"
          : "actual product initial paint/colors under unchanged CSP; no snapshot request",
      );

      const first = ui.live();
      await assertLiteralContent(ui, first);
      result.checks.push(
        "hostile terminal and message HTML/style/script/URL payloads remain literal text without styles, DOM escape or resources",
      );
      first.send(data(4, "\x1b[3;1HNEW-MARKER\x1b[5;7H\x1b[?25h"));
      await ui.rows().nth(2).getByText("NEW-MARKER", { exact: true }).waitFor();
      await ui.drawer.locator(".xterm-cursor").waitFor();
      // Partial sequence must not survive a semantic full reset.
      first.send(data(5, "\x1b]52;c;UNFINISHED"));
      first.send(
        full(6, {
          cols: 30,
          rows: 8,
          bytes: encode("\x1b[HRESET-CLEAN\x1b[4;15H\x1b[?25h"),
        }),
      );
      await ui
        .rows()
        .first()
        .getByText("RESET-CLEAN", { exact: true })
        .waitFor();
      assert.equal(await ui.rows().count(), 8);
      result.checks.push(
        "ordered overwrite/cursor and complete reset clears parser and changes viewport",
      );

      const beforeInput = ui.requests.length;
      const beforeInputBrowser = ui.browserRequests.length;
      await ui.page
        .getByRole("region", { name: "Read-only live terminal" })
        .focus();
      await ui.page.keyboard.type("NO HOST INPUT");
      await ui.page.keyboard.press("Enter");
      await ui.page.mouse.wheel(0, 100);
      first.send(
        data(
          7,
          "\x1b]0;BAD-TITLE\x07\x1b]52;c;" +
            "YQ==".repeat(20000) +
            "\x07\x1b]8;;https://peek.invalid/never\x07LINK\x1b]8;;\x07\x1b[8;99;199t\x1b[6n\r\nSAFE-AFTER",
        ),
      );
      // Large control string spans legitimate <=64 KiB data records below.
      // This record intentionally exceeds DATA_LIMIT, so source validation must stop it.
      await ui.drawer
        .getByText(
          "Live view isn’t available. Try again or choose Text snapshot.",
        )
        .waitFor();
      assert.equal(ui.screens(), 0);
      assert.deepEqual(ui.browserRequests.slice(beforeInputBrowser), []);
      assert.deepEqual(ui.requests.slice(beforeInput), []);
      result.checks.push(
        "keyboard/scroll emits no host requests; oversized delta is refused without snapshot fallback",
      );

      await ui.drawer
        .getByRole("button", { name: "Try again", exact: true })
        .click();
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      const guarded = ui.live();
      const titleBeforeEscapes = await ui.page.title();
      const sizeBeforeEscapes = await ui.page.evaluate(() => [
        innerWidth,
        innerHeight,
      ]);
      const controls =
        "\x1b]0;BAD-TITLE\x07\x1b]52;c;" +
        "A".repeat(100000) +
        "\x07\x1b]8;;https://peek.invalid/never\x07LINK\x1b]8;;\x07\x1b[8;99;199t\x1b[6n\r\nSAFE-AFTER";
      let seq = 2;
      for (let offset = 0; offset < controls.length; offset += 32000)
        guarded.send(data(seq++, controls.slice(offset, offset + 32000)));
      await ui.rows().getByText("SAFE-AFTER", { exact: true }).waitFor();
      assert.equal(
        (await ui.page.evaluate(() => window.peekPolicy)).clipboard,
        0,
      );
      assert.equal((await ui.page.evaluate(() => window.peekPolicy)).opened, 0);
      assert.equal(await ui.page.title(), titleBeforeEscapes);
      assert.deepEqual(
        await ui.page.evaluate(() => [innerWidth, innerHeight]),
        sizeBeforeEscapes,
      );
      assert.equal(await ui.drawer.locator("a, img").count(), 0);
      result.checks.push(
        "split 100k control string is contained across records; no clipboard/title/link/window effects",
      );

      const originalCount = ui.liveCount();
      guarded.res.end();
      await until(
        () => ui.liveCount() > originalCount,
        "expected a real network reconnect",
      );
      assert.ok(
        ui.requests
          .filter((r) => r.path.endsWith("/live"))
          .at(-1)
          .query.includes("continuity=browser-continuity"),
      );
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      await ui.chip("claude").click();
      await until(() => ui.live()?.member === "claude", "new target stream");
      await until(
        () =>
          ui.allStreams
            .filter((s) => s.member === "codex")
            .every((s) => s.ended),
        "old target sockets must close after cancellation",
      );
      assert.ok(
        ui.allStreams.filter((s) => s.member === "codex").every((s) => s.ended),
      );
      await ui.page.keyboard.press("Escape");
      await ui.drawer.waitFor({ state: "detached" });
      await until(() => ui.streams.size === 0, "close cancels stream");
      assert.ok(
        await ui
          .chip("claude")
          .evaluate((el) => el.contains(document.activeElement)),
      );
      result.checks.push(
        "reconnect preserves continuity; target switch/close abort old stream and restore focus",
      );

      ui.mode("unsupported");
      await clickMember(ui, "codex");
      await ui.page
        .getByRole("region", { name: "Screen text", exact: true })
        .getByText("SANITIZED TEXT SNAPSHOT")
        .waitFor();
      assert.equal(ui.screens(), 1);
      await ui.drawer
        .getByRole("button", { name: "Refresh", exact: true })
        .click();
      await until(() => ui.screens() === 2, "manual snapshot refresh");
      await ui.drawer
        .getByRole("button", { name: "Close screen", exact: true })
        .click();
      ui.mode("retry");
      await clickMember(ui, "codex");
      await ui.drawer
        .getByText(
          "Live view isn’t available. Try again or choose Text snapshot.",
        )
        .waitFor({ timeout: 15000 });
      assert.equal(ui.screens(), 2);
      result.checks.push(
        "unsupported startup auto snapshot/manual refresh; retry exhaustion never silently reads snapshot",
      );

      await ui.drawer
        .getByRole("button", { name: "Close screen", exact: true })
        .click();
      ui.mode("normal");
      await clickMember(ui, "codex");
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      ui.live().send({ ...projection(60, "LATEST-OUTPUT"), seq: 2 });
      await ui
        .rows()
        .last()
        .getByText("LATEST-OUTPUT", { exact: true })
        .waitFor();
      for (const [label, width, height, theme] of [
        ["desktop-dark", 1440, 900, "dark"],
        ["desktop-light", 1024, 700, "light"],
        ["mobile-dark", 390, 844, "dark"],
        ["mobile-light", 390, 844, "light"],
      ]) {
        await ui.page.setViewportSize({ width, height });
        await ui.page.emulateMedia({ colorScheme: theme });
        assert.equal(
          await ui.page.evaluate(
            () => document.documentElement.scrollWidth > innerWidth,
          ),
          false,
        );
        await ui.page.screenshot({
          path: join(output, `${name}-${label}.png`),
        });
      }
      const last = ui.live();
      last.send(outcome("replaced", "closed", 3));
      await ui.drawer
        .getByText(
          "This pane changed. Reopen the member to see its current screen.",
        )
        .waitFor();
      assert.equal(await ui.drawer.locator(".xterm").count(), 0);
      assert.equal(ui.screens(), 2);
      assert.deepEqual(ui.errors, []);
      assert.deepEqual(ui.external, []);
      assert.deepEqual(ui.dialogs, []);
      assert.deepEqual(
        await ui.page.evaluate(() => window.peekPolicy.violations),
        [],
      );
      result.checks.push(
        "desktop/light/mobile containment, replacement clears raw screen, no CSP/browser errors",
      );
      await ui.drawer
        .getByRole("button", { name: "Close screen", exact: true })
        .click();
      ui.mode("normal");
      await clickMember(ui, "codex");
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      ui.mode("session");
      ui.live().res.end();
      await ui.drawer
        .getByText(
          "Session ended. Open Namedrop’s dashboard link again to keep reading.",
        )
        .waitFor();
      assert.equal(await ui.drawer.locator(".xterm").count(), 0);
      assert.equal(ui.screens(), 2);
      result.checks.push(
        "session refusal on reconnect clears raw screen without snapshot fallback",
      );
      result.cspControls = await ui.cspControls();
      result.checks.push(
        "positive-controlled CSP blocks inline script/event/eval and remote script/connect/style/image; expected probes are separate from zero unexpected violations",
      );
      result.unexpectedComponentViolations = await ui.page.evaluate(
        () => window.peekPolicy.violations,
      );
      assert.deepEqual(result.unexpectedComponentViolations, []);
      result.status = "passed";
    } catch (error) {
      failed = true;
      result.status = "failed";
      result.error = String(error.stack ?? error);
      if (ui) {
        result.probeDetails = ui.probeDetails;
        result.policy = await ui.page.evaluate(() => window.peekPolicy);
        await ui.page.screenshot({ path: join(output, `${name}-failure.png`) });
      }
    } finally {
      await ui?.close();
      await browser.close();
    }
  }
  await writeFile(
    join(output, "results.json"),
    `${JSON.stringify(results, null, 2)}\n`,
  );
  assert.equal(failed, false, `Both browsers must pass. Evidence: ${output}`);
  console.log(
    styleDiagnostic
      ? "LIVE_PEEK_STYLE_DIAGNOSTIC_PASS"
      : "LIVE_PEEK_BROWSERS_PASS",
  );
});
