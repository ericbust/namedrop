import { type RefObject, useEffect } from "react";

export function useEscape(
  pane: RefObject<HTMLElement | null>,
  onClose: () => void,
): void {
  useEffect(() => {
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      const target = document.activeElement;
      if (target?.closest('dialog, [role="dialog"], [role="alertdialog"]'))
        return;
      const inside = target !== null && pane.current?.contains(target);
      const body =
        target === document.body &&
        ![
          ...document.querySelectorAll(
            'dialog[open], [role="dialog"], [role="alertdialog"]',
          ),
        ].some((dialog) => dialog.getClientRects().length > 0);
      if (inside || body) {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onEscape);
    return () => document.removeEventListener("keydown", onEscape);
  }, [pane, onClose]);
}
