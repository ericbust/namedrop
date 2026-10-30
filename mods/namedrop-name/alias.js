export function normalizeAlias(input) {
  let alias = "";
  let pendingSeparator = false;
  for (const character of input) {
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

export function aliasTaken(terminals, alias, address) {
  const target = terminals.find((terminal) => terminal.address === address);
  if (!target) return false;
  const space = target.labels?.["herdr.space"];
  return terminals.some((terminal) => terminal.address !== address
    && terminal.state === "live"
    && terminal.labels?.["herdr.space"] === space
    && terminal.labels?.["namedrop.name"] === alias);
}
