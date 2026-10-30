import type { Attention, ThreadAction } from "./model.ts";

// A thread control waits in `pending` after the server answers, because the
// frame that drops the item is the proof it worked.
export type ThreadControl =
  | { kind: "idle" }
  | { kind: "confirmingStop" }
  | { kind: "pending"; action: ThreadAction }
  | { kind: "failed"; action: ThreadAction; status: number };
export type ThreadControlEvent =
  | { type: "start"; action: ThreadAction }
  | { type: "ask-stop" }
  | { type: "cancel" }
  | { type: "failed"; status: number };

export function threadControl(
  state: ThreadControl,
  event: ThreadControlEvent,
): ThreadControl {
  switch (event.type) {
    case "start": {
      const ready =
        event.action === "stop"
          ? state.kind === "confirmingStop"
          : state.kind === "idle" || state.kind === "failed";
      return ready ? { kind: "pending", action: event.action } : state;
    }
    case "ask-stop":
      return state.kind === "idle" || state.kind === "failed"
        ? { kind: "confirmingStop" }
        : state;
    case "cancel":
      return state.kind === "confirmingStop" ? { kind: "idle" } : state;
    case "failed":
      return state.kind === "pending"
        ? { kind: "failed", action: state.action, status: event.status }
        : state;
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}

// The browser can continue or stop an item's thread only when the item names
// the thread and lists the action.
export function threadControls(
  item: Pick<Attention, "threadId" | "actions">,
): { threadId: string; actions: readonly ThreadAction[] } | null {
  return item.threadId === null || item.actions.length === 0
    ? null
    : { threadId: item.threadId, actions: item.actions };
}
