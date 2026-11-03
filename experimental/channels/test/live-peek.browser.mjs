import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright-core";

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

async function open(browser) {
  const requests = [],
    streams = new Set(),
    allStreams = [];
  let mode = "normal",
    screens = 0;
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    requests.push({
      path: url.pathname,
      query: url.search,
      method: req.method,
      cookie: req.headers.cookie,
    });
    if (url.pathname === "/api/events") {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.write(
        `event: space\ndata: ${JSON.stringify(fixture)}\n\nevent: ready\ndata: ${JSON.stringify({ keys: [fixture.key] })}\n\n`,
      );
      return;
    }
    if (url.pathname === "/api/session") {
      res.writeHead(200, { "Content-Type": "application/json" });
      return res.end(JSON.stringify({ csrf_token: "fixture-csrf" }));
    }
    if (url.pathname.endsWith("/screen")) {
      screens++;
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
      stream.send(full());
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
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext({
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
    if (route.request().url().startsWith(`${origin}/`)) return route.continue();
    external.push(route.request().url());
    return route.abort();
  });
  await page.addInitScript(() => {
    window.peekPolicy = { violations: [], clipboard: 0, opened: 0 };
    document.addEventListener("securitypolicyviolation", (e) =>
      window.peekPolicy.violations.push(e.violatedDirective),
    );
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
  });
  const navigation = await page.goto(`${origin}/w/${fixture.key}`);
  assert.ok(navigation);
  assert.equal(
    navigation.headers()["content-security-policy"],
    servedCsp,
    "Fixture response must serve the extracted product policy",
  );
  await page.getByRole("heading", { name: "alpha", exact: true }).waitFor();
  const chip = (member) =>
    page.locator(`[data-address-id="${member}"] .member`);
  const drawer = page.getByRole("complementary", { name: /^Screen of/ });
  const rows = () => drawer.locator(".xterm-rows > div");
  const live = () => allStreams.at(-1);
  const liveCount = () =>
    requests.filter((r) => r.path.endsWith("/live")).length;
  return {
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
    errors,
    external,
    dialogs,
    screens: () => screens,
    mode(value) {
      mode = value;
    },
    async close() {
      await context.close();
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
async function until(fn, message) {
  const deadline = Date.now() + 5000;
  while (!fn()) {
    if (Date.now() >= deadline) assert.fail(message);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("live Peek product component, transport and CSP in real Chromium and Firefox", {
  timeout: 120000,
}, async () => {
  const results = [];
  let failed = false;
  for (const [name, type, executablePath] of [
    ["chromium", chromium, process.env.CHROME ?? "/usr/bin/chromium"],
    ["firefox", firefox, process.env.FIREFOX ?? firefox.executablePath()],
  ]) {
    const browser = await type.launch({
      executablePath,
      headless: true,
      ...(name === "chromium" ? { args: ["--no-sandbox"] } : {}),
    });
    const result = {
      name,
      scope: styleDiagnostic
        ? "NON-GATING relaxed-style diagnostic"
        : "product CSP",
      csp: servedCsp,
      securitySource: securityEvidence,
      version: browser.version(),
      executablePath,
      checks: [],
      status: "running",
    };
    results.push(result);
    let ui;
    try {
      ui = await open(browser);
      await ui.chip("codex").click();
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
      assert.notEqual(colors[0], colors[1]);
      result.checks.push(
        styleDiagnostic
          ? "diagnostic initial paint/colors with relaxed style policy; no snapshot request"
          : "actual product initial paint/colors under unchanged CSP; no snapshot request",
      );

      const first = ui.live();
      first.send(data(2, "\x1b[3;1HNEW-MARKER\x1b[5;7H\x1b[?25h"));
      await ui.rows().nth(2).getByText("NEW-MARKER", { exact: true }).waitFor();
      await ui.drawer.locator(".xterm-cursor").waitFor();
      // Partial sequence must not survive a semantic full reset.
      first.send(data(3, "\x1b]52;c;UNFINISHED"));
      first.send(
        full(4, {
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
      await ui.page
        .getByRole("region", { name: "Read-only live terminal" })
        .focus();
      await ui.page.keyboard.type("NO HOST INPUT");
      await ui.page.keyboard.press("Enter");
      await ui.page.mouse.wheel(0, 100);
      first.send(
        data(
          5,
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
      await ui.chip("codex").click();
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
      await ui.chip("codex").click();
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
      await ui.chip("codex").click();
      await ui
        .rows()
        .first()
        .getByText("RED-SENTINEL", { exact: true })
        .waitFor();
      for (const [label, width, height, theme] of [
        ["desktop-dark", 1440, 900, "dark"],
        ["desktop-light", 1024, 700, "light"],
        ["mobile-dark", 390, 844, "dark"],
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
      last.send(outcome("replaced", "closed", 2));
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
      await ui.chip("codex").click();
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
      result.status = "passed";
    } catch (error) {
      failed = true;
      result.status = "failed";
      result.error = String(error.stack ?? error);
      if (ui) {
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
