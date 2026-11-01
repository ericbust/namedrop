export function initialModel() {
  return { dirty: false, reading: false, lastRead: null, snapshot: null, error: null, lastEnsure: null, timerAt: null };
}

export function watchedStoreFile(path) {
  return typeof path === "string" && path.startsWith("/") && path.slice(path.lastIndexOf("/") + 1).startsWith("namedrop.sqlite3");
}

export function transition(current, event) {
  const model = { ...current };
  const effects = [];
  if (event.type === "wake" || event.type === "retry") model.dirty = true;
  if (event.type === "readOk" || event.type === "readError") {
    model.reading = false;
    if (event.type === "readOk") {
      model.snapshot = event.snapshot;
      model.error = null;
    } else {
      model.error = event.error;
      if (model.lastEnsure === null || event.now - model.lastEnsure >= 30000) {
        model.lastEnsure = event.now;
        effects.push({ type: "ensure" });
      }
    }
  }
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
