import type { Attention, ControlAction, ThreadAction } from "./model.ts";

// The actions that ask once before they post, because they can't be undone.
export type ConfirmedAction = Extract<ControlAction, "stop" | "dismiss">;
export type ThreadControl =
  | { kind: "idle" }
  | { kind: "confirming"; action: ConfirmedAction }
  | { kind: "pending"; action: ControlAction }
  | { kind: "failed"; action: ControlAction; status: number };
export type ThreadControlEvent =
  | { type: "start"; action: ControlAction }
  | { type: "ask"; action: ConfirmedAction }
  | { type: "cancel" }
  | { type: "failed"; status: number };

function confirmed(action: ControlAction): action is ConfirmedAction {
  return action === "stop" || action === "dismiss";
}

export function threadControl(
  state: ThreadControl,
  event: ThreadControlEvent,
): ThreadControl {
  switch (event.type) {
    case "start": {
      const ready = confirmed(event.action)
        ? state.kind === "confirming" && state.action === event.action
        : state.kind === "idle" || state.kind === "failed";
      return ready ? { kind: "pending", action: event.action } : state;
    }
    case "ask":
      return state.kind === "idle" || state.kind === "failed"
        ? { kind: "confirming", action: event.action }
        : state;
    case "cancel":
      return state.kind === "confirming" ? { kind: "idle" } : state;
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

// The attempt whose warning the person can dismiss: only one whose held
// answer can never arrive, which is when the server allows it.
export function dismissal(
  item: Pick<Attention, "source" | "sourceId" | "answerLost">,
): string | null {
  return item.source === "agent_disposition" &&
    item.answerLost != null &&
    item.sourceId != null
    ? item.sourceId
    : null;
}
