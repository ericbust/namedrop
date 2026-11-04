import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "../../experimental/channels/node_modules/playwright-core/index.mjs";

const [urlFile, outputArgument] = process.argv.slice(2);
assert.ok(urlFile, "Pass the JSON receipt from isolated testbed namedrop channels --no-open");
const receipt = JSON.parse(await readFile(urlFile, "utf8"));
const url = new URL(receipt.data?.url ?? receipt.url);
assert.equal(url.hostname, "127.0.0.1");
const output = outputArgument ?? fileURLToPath(new URL("./live/", import.meta.url));
await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/chromium",
  headless: true,
  args: ["--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
const results = [];
const stamp = Date.now().toString(36);

async function read(path) {
  return page.evaluate(async (path) => {
    const response = await fetch(path, { credentials: "same-origin" });
    const text = await response.text();
    return { status: response.status, data: text ? JSON.parse(text) : null };
  }, path);
}

async function until(check, description, milliseconds = 180_000) {
  const deadline = Date.now() + milliseconds;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

try {
  await page.goto(url.toString());
  const peers = await until(async () => {
    const answer = await read("/api/direct-messages");
    if (answer.status !== 200) return null;
    const entries = answer.data.peers.filter((peer) => !peer.closed);
    return entries.filter((peer) => peer.alias.replace(/^@/, "") === "claude").length >= 2 &&
      entries.some((peer) => peer.alias.replace(/^@/, "") === "codex")
      ? entries : null;
  }, "two Claude peers and a Codex peer", 30_000);
  const claudes = peers.filter((peer) => peer.alias.replace(/^@/, "") === "claude");
  const first = claudes[0];
  const second = claudes.find((peer) => peer.space_key !== first.space_key);
  assert.ok(second, "Same-named Claude peers belong to different workspaces");
  const codex = peers.find((peer) => peer.alias.replace(/^@/, "") === "codex");
  const trials = [first, second, codex].map((peer, index) => ({
    peer,
    marker: `PRIVATE_DM_${stamp}_${index + 1}`,
    path: `/w/${peer.space_key}/direct-messages/${encodeURIComponent(peer.address_id)}/messages`,
  }));
  for (const trial of trials) {
    const { peer, marker, path } = trial;
    await page.locator(`[data-dm-key="${peer.space_key}/${peer.address_id}"]`).click();
    const composer = page.getByRole("textbox", { name: `Message @${peer.alias.replace(/^@/, "")} privately` });
    await composer.fill(`This is a private operator test. Use the bound reply operation to reply with exactly ${marker}. The text @other is literal and must not add a recipient.`);
    await composer.press("Escape");
    await composer.press("Enter");
    const history = await until(async () => {
      const answer = await read(path);
      return answer.status === 200 && answer.data.messages.some((message) =>
        message.author !== "human" && message.body.text.includes(marker)) ? answer.data : null;
    }, `real agent reply ${marker}`);
    await until(async () => (await page.locator("main").innerText()).includes(marker), "reply in selected DM", 15_000);
    for (const other of trials.filter((candidate) => candidate !== trial)) {
      const answer = await read(other.path);
      assert.equal(answer.status, 200);
      assert.equal(answer.data.messages.some((message) => message.body.text.includes(marker)), false);
    }
    const image = `${output}/dm-${results.length + 1}.png`;
    await page.screenshot({ path: image, fullPage: true });
    results.push({
      space_key: peer.space_key,
      address_id: peer.address_id,
      alias: peer.alias,
      workspace_label: peer.workspace_label,
      marker,
      message_ids: history.messages.map((message) => message.message_id),
      screenshot: image,
    });
    console.log(`Verified private reply for @${peer.alias.replace(/^@/, "")} in #${peer.workspace_label}`);
  }
  for (const [index, space] of [...new Set(trials.map((trial) => trial.peer.space_key))].entries()) {
    await page.goto(`${url.origin}/w/${space}`);
    const history = await read(`/w/${space}/messages?limit=100`);
    assert.equal(history.status, 200);
    assert.equal(trials.some((trial) => JSON.stringify(history.data).includes(trial.marker)), false);
    for (const result of results.filter((result) => result.space_key === space)) {
      for (const message of result.message_ids) {
        const context = await read(`/w/${space}/messages/${encodeURIComponent(message)}/context`);
        assert.equal(context.status, 404, "Channel context cannot read a DM by message ID");
      }
    }
    await page.screenshot({ path: `${output}/channel-${index + 1}-clean.png`, fullPage: true });
  }
  await page.goto(`${url.origin}/?view=activity`);
  await page.screenshot({ path: `${output}/all-activity-clean.png`, fullPage: true });
  assert.deepEqual(errors, [], "No uncaught browser errors");
  await writeFile(`${output}/results.json`, `${JSON.stringify({ passed: true, results }, null, 2)}\n`);
  console.log(`PASS. Evidence saved under ${output}`);
} catch (error) {
  await page.screenshot({ path: `${output}/failure.png`, fullPage: true }).catch(() => {});
  await writeFile(`${output}/results.json`, `${JSON.stringify({ passed: false, error: error.message, results, browser_errors: errors }, null, 2)}\n`);
  throw error;
} finally {
  await browser.close();
}
