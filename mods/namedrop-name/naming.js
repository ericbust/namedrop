import { normalizeAlias, aliasTaken, formatHcError } from "./alias.js";

export function chooseTarget(properties, focus) {
  return properties?.terminal ?? focus?.terminal ?? null;
}

export function isMissing(error) {
  return /^`herdr\.exec`: cannot run .+: .*No such file or directory \(os error 2\)/.test(String(error?.message ?? error));
}

export async function findBinary(exec, home) {
  const paths = [home() + "/.local/bin/namedrop-app", "namedrop"];
  for (const path of paths) {
    try {
      const result = await exec(path, ["--version"]);
      if (result.code !== 0) return { error: formatHcError(result.stderr ?? "") || `Namedrop version check failed with exit code ${result.code}.` };
      return { binary: path };
    } catch (error) {
      if (!isMissing(error)) return { error: String(error?.message ?? error) };
    }
  }
  return { missing: true };
}

export async function saveName({ target, alias, terminals, home, exec, hc }) {
  const snapshot = terminals();
  const terminal = snapshot.find((row) => row.address === target);
  if (terminal?.state !== "live") return { error: "This terminal is no longer live. Close this dialog and focus a live terminal." };
  const normalized = normalizeAlias(alias);
  if (normalized.error) return { error: normalized.error };
  if (aliasTaken(snapshot, normalized.alias, target)) return { error: "That name is already taken in this space." };
  const found = await findBinary(exec, home);
  if (found.error) return { error: found.error };
  if (found.missing) {
    try {
      const result = await hc(["label", target, "namedrop.name=" + normalized.alias]);
      return result.code === 0 ? { ok: true } : { error: formatHcError(result.stderr ?? "") || `Naming failed with exit code ${result.code}.` };
    } catch (error) {
      return { error: String(error?.message ?? error) };
    }
  }
  const type = terminal.status || terminal.program?.agent === true ? "agent" : "terminal";
  const args = ["--host", "hc", "--hc-terminal", target, "--hc-space", terminal.labels?.["herdr.space"] ?? "headless", "name", normalized.alias, "--type", type];
  try {
    const result = await exec(found.binary, args);
    if (result.code === 0) return { ok: true };
    if (result.code === 65) {
      try {
        const message = JSON.parse(result.stdout)?.error?.message;
        if (typeof message === "string") return { error: message };
      } catch {}
    }
    return { error: formatHcError(result.stderr ?? "") || `Naming failed with exit code ${result.code}.` };
  } catch (error) {
    return { error: String(error?.message ?? error) };
  }
}
