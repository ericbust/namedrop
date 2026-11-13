import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createCopy } from "../copy.ts";
import {
  type AgentDraft,
  type AgentRequest,
  type AgentResult,
  agentKinds,
  agentRequest,
  liveNameProblem,
} from "../create.ts";
import { CreateForm, useCreateForm } from "./CreateForm.tsx";

// `folder` reads where the channel's next agent opens.
export type AgentAdder = {
  add: (request: AgentRequest) => Promise<AgentResult>;
  folder: (signal: AbortSignal) => Promise<string | null>;
};

export function AddAgentForm({
  id,
  taken,
  adder,
  onClose,
}: {
  id: string;
  taken: ReadonlySet<string>;
  adder: AgentAdder;
  onClose: () => void;
}) {
  const [folder, setFolder] = useState<string | null>(null);
  // The form asks once, as it opens: the adder is made anew on every render.
  const asked = useRef(adder);
  useEffect(() => {
    const request = new AbortController();
    void asked.current.folder(request.signal).then((found) => {
      if (!request.signal.aborted) setFolder(found);
    });
    return () => request.abort();
  }, []);
  const [draft, setDraft] = useState<AgentDraft>({
    kind: "claude",
    name: "",
    persona: "",
  });
  const { state, submit, edit } = useCreateForm();
  const nameField = useRef<HTMLInputElement>(null);
  const kindName = useId();
  const nameId = useId();
  useLayoutEffect(() => nameField.current?.focus(), []);
  function change(next: Partial<AgentDraft>) {
    setDraft((now) => ({ ...now, ...next }));
    edit();
  }
  const live = liveNameProblem(draft.name, taken);
  return (
    <CreateForm
      id={id}
      label={createCopy.addAgent}
      className="add-agent"
      state={state}
      message={state.kind === "failed" ? state.message : live}
      submitLabel={createCopy.add}
      pendingLabel={createCopy.adding}
      onCancel={onClose}
      onSubmit={() =>
        submit(agentRequest(draft, taken), async (request) => {
          const result = await adder.add(request);
          if (result.kind !== "created") return result;
          onClose();
          return null;
        })
      }
    >
      <fieldset className="create-kinds">
        <legend className="create-label">{createCopy.kind}</legend>
        {agentKinds.map((kind) => (
          <label key={kind} className="create-kind">
            <input
              type="radio"
              name={kindName}
              value={kind}
              checked={draft.kind === kind}
              onChange={() => change({ kind })}
            />
            <span>{createCopy.kinds[kind]}</span>
          </label>
        ))}
      </fieldset>
      <div className="create-row">
        <div className="create-field">
          <label className="create-label" htmlFor={nameId}>
            {createCopy.agentName}
          </label>
          <span className="create-alias">
            <span aria-hidden="true">@</span>
            <input
              id={nameId}
              ref={nameField}
              type="text"
              autoComplete="off"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              placeholder="code-reviewer"
              aria-invalid={live !== null || undefined}
              value={draft.name}
              onChange={(event) =>
                change({ name: event.target.value.replace(/^@/, "") })
              }
            />
          </span>
        </div>
        <label className="create-field create-persona">
          <span className="create-label">
            {createCopy.persona}{" "}
            <span className="create-optional">({createCopy.optional})</span>
          </span>
          <input
            type="text"
            autoComplete="off"
            placeholder={createCopy.personaPlaceholder}
            value={draft.persona}
            onChange={(event) => change({ persona: event.target.value })}
          />
        </label>
      </div>
      {folder !== null && (
        <p className="create-opens-in">
          <span>{createCopy.opensIn}</span>
          <span className="create-folder" title={folder}>
            <bdi>{folder}</bdi>
          </span>
        </p>
      )}
    </CreateForm>
  );
}
