export function initialModel() {
  return { dirty: false, reading: false, lastRead: null, snapshot: null, error: null, lastEnsure: null, ensuring: false, timerAt: null, seen: {}, pending: [], overflow: 0, overflowLevel: null, lastToast: null, toastAt: null, errorNotified: null };
}

export function watchedStoreFile(path) {
  return typeof path === "string" && path.startsWith("/") && path.slice(path.lastIndexOf("/") + 1).startsWith("namedrop.sqlite3");
}

function queue(model, notice) {
  if (model.overflow || model.pending.length === 3) {
    model.overflow = Math.min(Number.MAX_SAFE_INTEGER, model.overflow + model.pending.length + 1);
    if (notice.level === "error" || model.pending.some((item) => item.level === "error")) model.overflowLevel = "error";
    model.pending = [];
  } else model.pending.push(notice);
}

function drain(model, now, effects) {
  if (!model.pending.length && !model.overflow) return;
  const due = model.lastToast === null ? now : model.lastToast + 1000;
  if (now < due) {
    model.toastAt = due;
    effects.push({ type: "toastTimer", delay: due - now });
    return;
  }
  const collapsed = model.overflow > 0;
  const notice = collapsed ? {
    message: `${model.overflow} more updates`, action: { name: "mod.open", args: { name: "namedrop" } },
  } : model.pending.shift();
  if (collapsed) {
    if (model.overflowLevel) notice.level = model.overflowLevel;
    model.overflow = 0;
    model.overflowLevel = null;
  }
  model.lastToast = now;
  model.toastAt = null;
  effects.push({ type: "toast", ...notice });
  if (model.pending.length || model.overflow) {
    model.toastAt = now + 1000;
    effects.push({ type: "toastTimer", delay: 1000 });
  }
}

function collect(model, snapshot) {
  const next = {};
  for (const row of snapshot.rows) {
    if (!row.terminal) continue;
    const key = JSON.stringify([snapshot.core, row.space, row.terminal]);
    const prior = model.seen[key];
    const replies = row.recent_replies ?? [];
    const latest = Math.max(0, ...replies.map((reply) => reply.at_ms));
    if (prior) {
      if (row.agent_status === "blocked" && prior.status !== "blocked") {
        queue(model, { message: `@${row.alias} is blocked`, action: { name: "agent.focus", args: { terminal: row.terminal } } });
      }
      const known = new Set(prior.ids);
      for (const reply of latest < prior.latest ? [] : [...replies].reverse()) {
        if (reply.at_ms < prior.latest || known.has(reply.message_id)) continue;
        queue(model, { message: `@${reply.from} replied to @${row.alias}`, action: { name: "agent.focus", args: { terminal: row.terminal } } });
      }
    }
    next[key] = { status: row.agent_status, latest, ids: replies.map((reply) => reply.message_id).slice(0, 5) };
  }
  model.seen = next;
}

export function transition(current, event) {
  const model = { ...current, pending: [...current.pending] };
  const effects = [];
  if (event.type === "wake" || event.type === "retry" || event.type === "start") model.dirty = true;
  if (event.type === "ensureDone") model.ensuring = false;
  if ((event.type === "start" || event.type === "readError") && !model.ensuring
      && (model.lastEnsure === null || event.now - model.lastEnsure >= 30000)) {
    model.lastEnsure = event.now;
    model.ensuring = true;
    effects.push({ type: "ensure" });
  }
  if (event.type === "readOk" || event.type === "readError" || event.type === "discoveryError") {
    model.reading = false;
    if (event.type === "readOk") {
      collect(model, event.snapshot);
      model.snapshot = event.snapshot;
      model.error = null;
    } else {
      model.error = event.error;
      if (event.error !== model.errorNotified) {
        model.errorNotified = event.error;
        queue(model, { message: event.error, level: "error", action: { name: "mod.open", args: { name: "namedrop" } } });
      }
    }
  }
  if (event.type === "toastTick" && event.at === model.toastAt) drain(model, event.now, effects);
  if (event.type === "readOk" || event.type === "readError" || event.type === "discoveryError") drain(model, event.now, effects);
  if (event.type === "timer" && event.at !== model.timerAt) return { model, effects };
  if (!model.dirty || model.reading) return { model, effects };
  const due = model.lastRead === null ? event.now : model.lastRead + 500;
  if (event.now >= due) {
    model.dirty = false;
    model.reading = true;
    model.lastRead = event.now;
    model.timerAt = null;
    effects.push({ type: "read" });
  } else {
    model.timerAt = due;
    effects.push({ type: "timer", delay: due - event.now });
  }
  return { model, effects };
}
