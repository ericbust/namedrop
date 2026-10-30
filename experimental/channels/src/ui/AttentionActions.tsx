import { type KeyboardEvent, useLayoutEffect, useRef, useState } from "react";
import {
  type AttentionNavigation,
  attentionConversation,
  messageIntent,
} from "../attention.ts";
import {
  type ThreadControl,
  type ThreadControlEvent,
  threadControl,
  threadControls,
} from "../control.ts";
import { threadControlCopy } from "../copy.ts";
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
) => Promise<ControlResult>;
const idle: ThreadControl = { kind: "idle" };
export function AttentionActions({
  item,
  channel,
  onFocus,
  onControl,
}: {
  item: Attention;
  channel: Pick<ChannelSnapshot, "key" | "members" | "messages">;
  onFocus: FocusAttention;
  onControl: ControlThread;
}) {
  const target = attentionConversation(item, channel);
  const response = item.response;
  const member =
    response && "addressId" in response
      ? channel.members.get(response.addressId)
      : undefined;
  const subject = item.subjectAlias ?? member?.alias;
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
  const peek = member === undefined ? null : peekTarget(channel.key, member);
  const controls = threadControls(item);
  const [control, setControl] = useState(idle);
  const busy = control.kind === "pending";
  const stopButton = useRef<HTMLButtonElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const shown = useRef(control.kind);
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
    void onControl(channel.key, controls.threadId, next.action).then(
      (result) => {
        if (result.kind === "failed")
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
      <div className="attention-actions">
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
            aria-label={member ? `Respond to ${member.alias}` : undefined}
            aria-disabled={busy}
            onClick={(event) => {
              if (busy) return;
              if (response.kind === "reply")
                onFocus(
                  channel.key,
                  { kind: "context", target: response, response },
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
            Respond
          </button>
        )}
        {response?.kind === "pane" && (
          <>
            <span>
              Respond in{" "}
              {member?.alias ?? item.subjectAlias ?? response.addressId}
              {member?.paneLabel ? ` · ${member.paneLabel.text}` : ""} in Herdr.
            </span>
            {peek !== null && (
              <PeekControl
                target={peek}
                className="text-button"
                fallback="none"
              >
                Peek
              </PeekControl>
            )}
          </>
        )}
        {response?.kind === "local" && (
          <span>
            {item.source === "integration_registration"
              ? "Open this workspace in Herdr and choose Namedrop setup from the command palette to repair the integration."
              : "Open this workspace in Herdr and choose Namedrop Doctor to review this item."}
          </span>
        )}
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
