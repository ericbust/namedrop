import { useEffect, useRef, useState } from "react";
import type { PeekTarget } from "../model.ts";
import { type PeekInput, sendPeekInput } from "../peekInput.ts";

type State =
  | { kind: "off" }
  | { kind: "ready"; text: string }
  | { kind: "sending"; text: string }
  | { kind: "error"; text: string; message: string };
type KeyInput = Extract<PeekInput, { kind: "key" }>["key"];
const keys: readonly { key: KeyInput; label: string }[] = [
  { key: "Escape", label: "Esc" },
  { key: "Ctrl-C", label: "Ctrl-C" },
  { key: "ArrowUp", label: "↑" },
  { key: "ArrowDown", label: "↓" },
  { key: "Enter", label: "Enter" },
];

export function PeekCompose({
  target,
  continuity,
}: {
  target: PeekTarget;
  continuity: string | null;
}) {
  const [state, setState] = useState<State>({ kind: "off" });
  const field = useRef<HTMLTextAreaElement>(null);
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => pending.current?.abort(), []);
  const active = continuity !== null && state.kind !== "off";
  async function send(
    input: { kind: "text"; text: string } | { kind: "key"; key: KeyInput },
  ) {
    if (continuity === null || state.kind === "off" || state.kind === "sending")
      return;
    const text = state.text;
    const controller = new AbortController();
    pending.current = controller;
    setState({ kind: "sending", text });
    try {
      await sendPeekInput(target, { ...input, continuity }, controller.signal);
      if (!controller.signal.aborted)
        setState({ kind: "ready", text: input.kind === "text" ? "" : text });
    } catch (error) {
      if (!controller.signal.aborted)
        setState({
          kind: "error",
          text,
          message:
            error instanceof Error ? error.message : "Input could not be sent",
        });
    } finally {
      if (pending.current === controller) pending.current = null;
      if (!controller.signal.aborted) field.current?.focus();
    }
  }
  function stop() {
    pending.current?.abort();
    pending.current = null;
    setState({ kind: "off" });
  }
  return (
    <fieldset
      className={`peek-compose${active ? " is-active" : ""}`}
      aria-label="Agent input"
      onKeyDown={(event) => {
        if (active && event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          void send({ kind: "key", key: "Escape" });
        }
      }}
    >
      {!active ? (
        <button
          type="button"
          className="peek-type-toggle"
          disabled={continuity === null}
          onClick={() => {
            setState({ kind: "ready", text: "" });
            requestAnimationFrame(() => field.current?.focus());
          }}
        >
          Type to agent
        </button>
      ) : (
        <>
          <div className="peek-compose-head">
            <span>Typing to {target.alias}</span>
            <button type="button" onClick={stop}>
              Stop typing
            </button>
          </div>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              if (state.kind === "sending" || !state.text.trim()) return;
              if (new TextEncoder().encode(state.text).length > 4096) {
                setState({
                  kind: "error",
                  text: state.text,
                  message: "Prompt is too long",
                });
              } else {
                void send({ kind: "text", text: state.text });
              }
            }}
          >
            <textarea
              ref={field}
              aria-label={`Message to ${target.alias}`}
              placeholder="Type a prompt, then press Enter"
              maxLength={4096}
              rows={2}
              value={state.text}
              disabled={state.kind === "sending"}
              onChange={(event) =>
                setState({
                  kind: "ready",
                  text: event.target.value.replace(/[\r\n]/g, " "),
                })
              }
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  event.currentTarget.form?.requestSubmit();
                }
              }}
            />
            <button
              type="submit"
              disabled={state.kind === "sending" || !state.text.trim()}
            >
              Send
            </button>
          </form>
          <fieldset className="peek-compose-keys" aria-label="Agent keys">
            {keys.map(({ key, label }) => (
              <button
                key={key}
                type="button"
                disabled={state.kind === "sending"}
                aria-label={`Send ${key}`}
                onClick={() => void send({ kind: "key", key })}
              >
                {label}
              </button>
            ))}
          </fieldset>
          {state.kind === "error" && (
            <p role="alert">{state.message}. Try again.</p>
          )}
        </>
      )}
    </fieldset>
  );
}
