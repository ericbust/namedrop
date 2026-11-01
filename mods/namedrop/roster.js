const priority = { blocked: 0, working: 1, done: 2, idle: 2 };

export function roster(terminals, panes, spaces, snapshot = null) {
  const stored = new Map((snapshot?.rows ?? []).filter((row) => row.terminal).map((row) => [row.terminal, row]));
  const shown = new Set(panes.map((pane) => pane.terminal));
  const locations = new Map(spaces.map((space) => [
    space.id,
    new Map(space.tabs.map((tab) => [tab.id, `${space.name} / ${tab.name}`])),
  ]));

  return terminals
    .filter((terminal) => terminal.state === "live" && (terminal.labels?.["namedrop.name"] || stored.has(terminal.address)))
    .map((terminal) => {
      const space = terminal.labels["herdr.space"];
      const tab = terminal.labels["herdr.tab"];
      const machine = terminal.address.includes("/")
        ? terminal.address.slice(0, terminal.address.indexOf("/") + 1)
        : "";
      const saved = stored.get(terminal.address);
      return {
        alias: saved?.alias ?? terminal.labels["namedrop.name"],
        address: terminal.address,
        status: terminal.status === "error" ? "blocked" : terminal.status ?? null,
        program: terminal.program ?? null,
        shown: shown.has(terminal.address),
        where: saved?.waiting_on?.length ? `waiting on @${saved.waiting_on[0].alias}`
          : space && tab ? locations.get(machine + space)?.get(machine + tab) ?? null : null,
      };
    })
    .sort((a, b) => (priority[a.status] ?? 3) - (priority[b.status] ?? 3)
      || (a.alias < b.alias ? -1 : a.alias > b.alias ? 1 : 0));
}
