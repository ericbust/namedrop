export function initialModel() {
  return { dirty: false, reading: false, lastRead: null, snapshot: null, error: null, lastEnsure: null, timerAt: null, seen: {}, pending: [], overflow: 0, lastToast: null, toastAt: null, errorNotified: null };
}

export function watchedStoreFile(path) {
  return typeof path === "string" && path.startsWith("/") && path.slice(path.lastIndexOf("/") + 1).startsWith("namedrop.sqlite3");
}

function queue(model, notice) {
  if (model.pending.length < 3) model.pending.push(notice);
  else model.overflow += 1;
}

function drain(model, now, effects) {
  if (!model.pending.length && !model.overflow) return;
  const due = model.lastToast === null ? now : model.lastToast + 1000;
  if (now < due) {
    model.toastAt = due;
    effects.push({ type: "toastTimer", delay: due - now });
    return;
  }
  const collapsed = model.pending.length === 0;
  const notice = collapsed ? {
    message: `${model.overflow} more updates`, action: { name: "mod.open", args: { name: "namedrop" } },
  } : model.pending.shift();
  if (collapsed) model.overflow = 0;
  model.lastToast = now;
  model.toastAt = null;
  effects.push({ type: "toast", ...notice });
  if (model.pending.length || model.overflow) {
    model.toastAt = now + 1000;
    effects.push({ type: "toastTimer", delay: 1000 });
  }
}

function collect(model, snapshot) {
  const next = { ...model.seen };
  for (const row of snapshot.rows) {
    const key = JSON.stringify([snapshot.core, row.space, row.terminal]);
    const prior = next[key];
    const replies = row.recent_replies ?? [];
    if (prior) {
      if (row.agent_status === "blocked" && prior.status !== "blocked") {
        queue(model, { message: `@${row.alias} is blocked`, action: { name: "agent.focus", args: { terminal: row.terminal } } });
      }
      const known = new Set(prior.ids);
      for (const reply of [...replies].reverse()) {
        if (reply.at_ms < prior.latest || known.has(reply.message_id)) continue;
        queue(model, { message: `@${reply.from} replied to @${row.alias}`, action: { name: "agent.focus", args: { terminal: row.terminal } } });
      }
    }
    const latest = Math.max(prior?.latest ?? 0, ...replies.map((reply) => reply.at_ms));
    next[key] = { status: row.agent_status, latest, ids: replies.map((reply) => reply.message_id).slice(0, 5) };
  }
  const keys = Object.keys(next);
  for (const key of keys.slice(0, Math.max(0, keys.length - 256))) delete next[key];
  model.seen = next;
}

export function transition(current, event) {
  const model = { ...current, pending: [...current.pending] };
  const effects = [];
  if (event.type === "wake" || event.type === "retry") model.dirty = true;
  if (event.type === "readOk" || event.type === "readError") {
    model.reading = false;
    if (event.type === "readOk") {
      collect(model, event.snapshot);
      model.snapshot = event.snapshot;
      model.error = null;
    } else {
      model.error = event.error;
      if (event.error !== model.errorNotified) {
        model.errorNotified = event.error;
        queue(model, { message: event.error, action: { name: "mod.open", args: { name: "namedrop" } } });
      }
      if (model.lastEnsure === null || event.now - model.lastEnsure >= 30000) {
        model.lastEnsure = event.now;
        effects.push({ type: "ensure" });
      }
    }
  }
  if (event.type === "toastTick" && event.at === model.toastAt) drain(model, event.now, effects);
  if (event.type === "readOk" || event.type === "readError") drain(model, event.now, effects);
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
