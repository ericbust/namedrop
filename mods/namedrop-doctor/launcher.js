const popup = "doctor";
const entries = { management: "manage", "agent-status": "status", doctor: "doctor", tour: "tour" };

function errorMessage(result) {
  try { return JSON.parse(result.stdout)?.error ?? null; } catch { return null; }
}

export async function launchPopup({ home, focused, terminals, exec, openOverlay, props = {} }) {
  const target = props.terminal ?? focused()?.terminal;
  const selected = terminals().find((terminal) => terminal.address === target && terminal.state === "live");
  if (!selected) return { error: "Focus a live terminal first." };
  const stable = home() + "/.local/bin/namedrop-app";
  let binary = null;
  for (const candidate of [stable, "namedrop"]) {
    try {
      const result = await exec(candidate, ["--version"]);
      if (result.code !== 0) return { error: result.stderr || `Namedrop version check failed with exit code ${result.code}.` };
      binary = candidate;
      break;
    } catch (error) {
      if (!/^`herdr\.exec`: cannot run .+: .*No such file or directory \(os error 2\)/.test(String(error?.message ?? error))) {
        return { error: String(error?.message ?? error) };
      }
    }
  }
  if (!binary) return { error: "Install the Namedrop binary to open this window." };
  const name = "namedrop-" + popup;
  const args = ["--host", "hc", "run", "--bg", "--json", "--rm", "--name", name, "--cwd", home(), "--", binary,
    "--host", "hc", "--hc-terminal", target, "--hc-space", selected.labels?.["herdr.space"] ?? "headless", "ui", "run", entries[popup]];
  let result;
  try { result = await exec(binary, args); }
  catch (error) { return { error: String(error?.message ?? error) }; }
  const failure = errorMessage(result);
  let address;
  if (result.code === 0) {
    try { address = JSON.parse(result.stdout).address; } catch { return { error: "Invalid launcher response." }; }
  } else if (failure?.kind === "name_taken" || failure?.code === "name_taken") {
    address = terminals().find((terminal) => terminal.name === name && terminal.state === "live")?.address;
    if (!address) return { error: failure.message ?? "Existing window is no longer live." };
  } else {
    return { error: failure?.message ?? result.stderr ?? `Launcher failed with exit code ${result.code}.` };
  }
  if (typeof address !== "string") return { error: "Launcher did not return a terminal address." };
  openOverlay({ kind: "window", key: name, width: 720, height: 500, layout: { type: "column", children: [
    { type: "block", size: { px: 28 }, content: { mod: name, props: { part: "bar" } } },
    { type: "block", content: { terminal: address } },
  ] } });
  return { ok: true };
}
