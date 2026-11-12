import {
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { createCopy, createFailureCopy } from "../copy.ts";
import {
  type Checked,
  editing,
  type FormState,
  formState,
  type Refusal,
} from "../create.ts";

// Runs one form's press: the draft's own check first, then the server, whose
// refusal lands in the form unless it closed meanwhile.
export function useCreateForm() {
  const [state, setState] = useState<FormState>(editing);
  const mounted = useRef(true);
  useEffect(
    () => () => {
      mounted.current = false;
    },
    [],
  );
  const submit = <T,>(
    checked: Checked<T>,
    run: (request: T) => Promise<Refusal | null>,
  ) => {
    if (state.kind === "pending") return;
    setState(
      formState(state, {
        type: "submit",
        problem: checked.kind === "problem" ? checked.message : null,
      }),
    );
    if (checked.kind !== "ok") return;
    void run(checked.request).then((refusal) => {
      if (refusal !== null && mounted.current)
        setState((now) =>
          formState(now, {
            type: "refused",
            message: createFailureCopy(refusal),
          }),
        );
    });
  };
  const edit = useCallback(
    () => setState((now) => formState(now, { type: "edit" })),
    [],
  );
  return { state, submit, edit };
}

// The page's CSP has `form-action 'none'`, so the form never submits natively.
export function CreateForm({
  id,
  label,
  className,
  state,
  message,
  submitLabel,
  pendingLabel,
  onSubmit,
  onCancel,
  children,
}: {
  id: string;
  label: string;
  className: string;
  state: FormState;
  message: string | null;
  submitLabel: string;
  pendingLabel: string;
  onSubmit: () => void;
  onCancel: () => void;
  children: ReactNode;
}) {
  const pending = state.kind === "pending";
  return (
    <form
      id={id}
      className={`create-form ${className}`}
      aria-label={label}
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || event.nativeEvent.isComposing) return;
        // The phone drawer and the panes close on Escape too; the form goes first.
        event.preventDefault();
        event.stopPropagation();
        onCancel();
      }}
    >
      {children}
      {message !== null && (
        <p className="create-failure" role="alert">
          {message}
        </p>
      )}
      <div className="create-actions">
        <button
          type="submit"
          className="create-submit"
          aria-disabled={pending}
          aria-busy={pending}
        >
          {pending ? pendingLabel : submitLabel}
        </button>
        <button type="button" className="create-cancel" onClick={onCancel}>
          {createCopy.cancel}
        </button>
      </div>
    </form>
  );
}
