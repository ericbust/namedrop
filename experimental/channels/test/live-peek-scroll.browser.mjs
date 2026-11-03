import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { lstat, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Firefox's Juggler headless agent sheet forces scrollbar-width:none!important.
// Use an unmodified Firefox on a private in-memory display for native thumb input.
export async function isolatedFirefoxDisplay(t, output) {
  if (process.platform !== "linux") return { headless: false };
  const directory = await mkdtemp(join(tmpdir(), "namedrop-peek-display-"));
  const home = join(directory, "home");
  await mkdir(home, { mode: 0o700 });
  const marker = join(directory, "fixture.json");
  await writeFile(
    marker,
    JSON.stringify({ purpose: "live-peek-scroll", pid: process.pid }),
  );
  assert.equal((await lstat(directory)).mode & 0o777, 0o700);
  const env = {
    PATH: process.env.PATH,
    LANG: "C.UTF-8",
    HOME: home,
    XDG_RUNTIME_DIR: directory,
    XDG_CONFIG_HOME: join(home, "config"),
    XDG_DATA_HOME: join(home, "data"),
    XDG_CACHE_HOME: join(home, "cache"),
  };
  const socket = "peek-test";
  const child = spawn(
    "weston",
    [
      "--backend=headless",
      "--renderer=pixman",
      "--no-config",
      `--socket=${socket}`,
      "--idle-time=0",
      "--width=1600",
      "--height=1000",
      `--log=${join(output, "firefox-display.log")}`,
    ],
    { env, stdio: "ignore" },
  );
  const exited = once(child, "exit").catch(() => null);
  let launchError;
  child.on("error", (error) => {
    launchError = error;
  });
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGTERM");
    await exited;
    await rm(directory, { recursive: true, force: true });
  });
  const deadline = Date.now() + 5000;
  let ready = false;
  while (Date.now() < deadline && !launchError && child.exitCode === null) {
    const info = await lstat(join(directory, socket)).catch(() => null);
    if (info?.isSocket()) {
      ready = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.ok(
    ready,
    `Isolated Weston failed: ${launchError ?? "see firefox-display.log"}`,
  );
  assert.ok(!("DISPLAY" in env) && !("WAYLAND_DISPLAY" in env));
  return {
    headless: false,
    env: { ...env, WAYLAND_DISPLAY: socket, MOZ_ENABLE_WAYLAND: "1" },
    firefoxUserPrefs: { "widget.gtk.overlay-scrollbars.enabled": false },
  };
}
