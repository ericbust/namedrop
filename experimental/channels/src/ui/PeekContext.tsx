import { createContext, type ReactNode, useContext } from "react";
import type { PeekTarget, SpaceKey } from "../model.ts";
import { peekKey } from "../peek.ts";

export type PeekAccess = Readonly<{
  scope: SpaceKey | null;
  open(target: PeekTarget, opener: HTMLElement, startedAt: number): void;
}>;
export const PeekContext = createContext<PeekAccess | null>(null);
export function usePeek(): PeekAccess | null {
  return useContext(PeekContext);
}
export function PeekControl({
  target,
  className,
  title,
  expanded,
  fallback,
  children,
}: {
  target: PeekTarget | null;
  className?: string;
  title?: string;
  expanded?: boolean;
  fallback: "span" | "none";
  children: ReactNode;
}) {
  const access = usePeek();
  if (target === null || access === null)
    return fallback === "span" ? (
      <span className={className} title={title}>
        {children}
      </span>
    ) : (
      children
    );
  return (
    <button
      type="button"
      className={className}
      title={title}
      data-peek={peekKey(target)}
      aria-expanded={expanded}
      onClick={(event) =>
        access.open(target, event.currentTarget, event.timeStamp)
      }
    >
      {children}
    </button>
  );
}
