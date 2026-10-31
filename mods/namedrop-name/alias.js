export function normalizeAlias(input) {
  const supplied = input.trim().replace(/^@/, "");
  let alias = "";
  let pendingSeparator = false;
  for (const character of supplied) {
    if (/^[A-Za-z0-9]$/.test(character)) {
      if (pendingSeparator && alias) alias += "-";
      alias += character.toLowerCase();
      pendingSeparator = false;
    } else if (/^[\t\n\f\r _-]$/.test(character)) {
      pendingSeparator = alias.length > 0;
    } else {
      return { error: "aliases can contain only ASCII letters, digits, spaces, hyphens, or underscores" };
    }
  }
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(alias)) {
    return { error: "alias must be lowercase ASCII kebab-case with no leading or trailing hyphen" };
  }
  return { alias };
}

function machinePrefix(address) {
  return address.slice(0, address.indexOf("/") + 1);
}

export function aliasTaken(terminals, alias, address) {
  const target = terminals.find((terminal) => terminal.address === address);
  if (!target) return false;
  const space = target.labels?.["herdr.space"];
  const machine = machinePrefix(address);
  return terminals.some((terminal) => terminal.address !== address
    && terminal.state === "live"
    && machinePrefix(terminal.address) === machine
    && terminal.labels?.["herdr.space"] === space
    && terminal.labels?.["namedrop.name"] === alias);
}

export function formatHcError(stderr) {
  const text = stderr.trim();
  try {
    const error = JSON.parse(text)?.error;
    if (typeof error?.message !== "string"
      || (error.hint !== undefined && typeof error.hint !== "string")) return text;
    return error.message + (error.hint ? " " + error.hint : "");
  } catch {
    return text;
  }
}
