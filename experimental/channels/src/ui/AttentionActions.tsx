import {
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import {
  type AttentionNavigation,
  attentionConversation,
  messageIntent,
  responderOf,
} from "../attention.ts";
import {
  type ThreadControl,
  type ThreadControlEvent,
  threadControl,
  threadControls,
} from "../control.ts";
import { responseCopy, threadControlCopy } from "../copy.ts";
import type {
  Attention,
  ChannelSnapshot,
  SpaceKey,
  ThreadAction,
} from "../model.ts";
import { peekTarget } from "../peek.ts";
import type { ControlResult } from "../send.ts";
import { PeekControl } from "./PeekContext.tsx";

export type FocusAttention = (
  key: SpaceKey,
  navigation: AttentionNavigation,
  button: HTMLButtonElement,
) => void;
export type ControlThread = (
  key: SpaceKey,
  threadId: string,
  action: ThreadAction,
  signal: AbortSignal,
) => Promise<ControlResult>;
const idle: ThreadControl = { kind: "idle" };
// A label wider than the whole row ends in an ellipsis, so its tooltip carries it whole.
function useWholeLabels(row: RefObject<HTMLElement | null>) {
  useEffect(() => {
    const element = row.current;
    if (element === null) return;
    const label = () => {
      for (const button of element.querySelectorAll("button")) {
        const ours = button.hasAttribute("data-cut");
        if (button.hasAttribute("title") && !ours) continue;
        const cut = button.scrollWidth > button.clientWidth;
        button.toggleAttribute("data-cut", cut);
        if (cut) button.title = button.textContent ?? "";
        else if (ours) button.removeAttribute("title");
      }
    };
    const resized = new ResizeObserver(label);
    const changed = new MutationObserver(label);
    resized.observe(element);
    changed.observe(element, {
      subtree: true,
      childList: true,
      characterData: true,
    });
    return () => {
      resized.disconnect();
      changed.disconnect();
    };
  }, [row]);
}
export function AttentionActions({
  item,
  channel,
  onFocus,
  onControl,
  children,
}: {
  item: Attention;
  channel: Pick<ChannelSnapshot, "key" | "members" | "messages">;
  onFocus: FocusAttention;
  onControl: ControlThread;
  children?: ReactNode;
}) {
  const target = attentionConversation(item, channel);
  const response = item.response;
  const member = responderOf(item, channel.members);
  const subject = item.subjectAlias ?? member?.alias;
  const responder = member?.alias ?? item.subjectAlias ?? "the agent";
  const navigation: AttentionNavigation | null =
    target === null
      ? null
      : item.conversation
        ? {
            kind: "context",
            target,
            response:
              response?.kind === "pane"
                ? response
                : messageIntent(item, target, false),
          }
        : { kind: "loaded", target };
  const peek = member === null ? null : peekTarget(channel.key, member);
  const controls = threadControls(item);
  const [control, setControl] = useState(idle);
  const busy = control.kind === "pending";
  const stopButton = useRef<HTMLButtonElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const shown = useRef(control.kind);
  const press = useRef<AbortController | null>(null);
  useEffect(() => () => press.current?.abort(), []);
  const row = useRef<HTMLDivElement>(null);
  useWholeLabels(row);
  useLayoutEffect(() => {
    const before = shown.current;
    shown.current = control.kind;
    if (before === control.kind) return;
    if (control.kind === "confirmingStop") confirmButton.current?.focus();
    else if (
      control.kind !== "pending" &&
      (before === "confirmingStop" || document.activeElement === document.body)
    )
      stopButton.current?.focus();
  });
  function act(event: ThreadControlEvent) {
    const next = threadControl(control, event);
    if (next === control) return;
    setControl(next);
    if (next.kind !== "pending" || controls === null) return;
    press.current?.abort();
    press.current = new AbortController();
    const { signal } = press.current;
    void onControl(channel.key, controls.threadId, next.action, signal).then(
      (result) => {
        if (result.kind === "failed" && !signal.aborted)
          setControl((now) =>
            threadControl(now, { type: "failed", status: result.status }),
          );
      },
    );
  }
  const cancelOnEscape = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    act({ type: "cancel" });
  };
  const confirming =
    control.kind === "confirmingStop" ||
    (control.kind === "pending" && control.action === "stop");
  return (
    <>
      <div className="attention-actions" ref={row}>
        {navigation !== null && (
          <button
            type="button"
            className="text-button"
            aria-label={subject ? `Show message from ${subject}` : undefined}
            aria-disabled={busy}
            onClick={(event) =>
              !busy && onFocus(channel.key, navigation, event.currentTarget)
            }
          >
            Show message
          </button>
        )}
        {controls !== null &&
          (confirming ? (
            <>
              <span className="attention-confirm">
                {threadControlCopy.confirm}
              </span>
              <button
                ref={confirmButton}
                type="button"
                className="text-button confirm-stop"
                aria-disabled={busy}
                onClick={() => act({ type: "start", action: "stop" })}
                onKeyDown={cancelOnEscape}
              >
                {busy
                  ? threadControlCopy.stop.pending
                  : threadControlCopy.confirmStop}
              </button>
              <button
                type="button"
                className="text-button"
                aria-disabled={busy}
                onClick={() => act({ type: "cancel" })}
                onKeyDown={cancelOnEscape}
              >
                {threadControlCopy.cancel}
              </button>
            </>
          ) : (
            <>
              {controls.actions.includes("continue") && (
                <button
                  type="button"
                  className="text-button"
                  title={threadControlCopy.continue.title}
                  aria-disabled={busy}
                  onClick={() => act({ type: "start", action: "continue" })}
                >
                  {busy
                    ? threadControlCopy.continue.pending
                    : threadControlCopy.continue.label}
                </button>
              )}
              {controls.actions.includes("stop") && (
                <button
                  ref={stopButton}
                  type="button"
                  className="text-button"
                  title={threadControlCopy.stop.title}
                  aria-disabled={busy}
                  onClick={() => act({ type: "ask-stop" })}
                >
                  {threadControlCopy.stop.label}
                </button>
              )}
            </>
          ))}
        {(response?.kind === "reply" ||
          (response?.kind === "message" && target !== null)) && (
          <button
            type="button"
            className="text-button"
            aria-disabled={busy}
            onClick={(event) => {
              if (busy) return;
              if (response.kind === "reply")
                onFocus(
                  channel.key,
                  {
                    kind: "context",
                    target: response,
                    response: { ...response, subject: item.subjectAlias },
                  },
                  event.currentTarget,
                );
              else if (target !== null)
                onFocus(
                  channel.key,
                  {
                    kind: "context",
                    target,
                    response: messageIntent(item, target, true),
                  },
                  event.currentTarget,
                );
            }}
          >
            {responseCopy[response.kind](responder)}
          </button>
        )}
        {response?.kind === "pane" && peek !== null && member && (
          <PeekControl target={peek} className="text-button" fallback="none">
            {responseCopy.pane(member.alias)}
          </PeekControl>
        )}
        {children}
      </div>
      {control.kind === "failed" && (
        <p
          className="attention-failure"
          role="alert"
          title={
            control.status > 0
              ? threadControlCopy.status(control.status)
              : undefined
          }
        >
          {control.status === 404
            ? threadControlCopy.gone
            : threadControlCopy[control.action].failed}
        </p>
      )}
    </>
  );
}
