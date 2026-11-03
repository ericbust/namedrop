export const panelStorageKey = "namedrop:panel-widths:v1";
export type PanelWidths = { left: number; right: number };
export const panelMinimum = { left: 180, right: 280 };
export const centerMinimum = 320;
export function clampWidth(value: number, minimum: number, maximum: number) {
  return Math.round(Math.max(minimum, Math.min(value, maximum)));
}
export function readPanelWidths(raw: string | null): Partial<PanelWidths> {
  try {
    const value: unknown = JSON.parse(raw ?? "null");
    if (typeof value !== "object" || value === null) return {};
    const valid = (width: unknown): width is number =>
      typeof width === "number" && Number.isFinite(width) && width > 0;
    return {
      ...("left" in value && valid(value.left) ? { left: value.left } : {}),
      ...("right" in value && valid(value.right) ? { right: value.right } : {}),
    };
  } catch {
    return {};
  }
}
export function panelWidths(
  preferred: Partial<PanelWidths>,
  viewport: number,
  open: boolean,
): PanelWidths {
  const left = clampWidth(
    preferred.left ?? (viewport <= 1100 ? 220 : 248),
    panelMinimum.left,
    Math.min(360, viewport - centerMinimum - (open ? panelMinimum.right : 0)),
  );
  const right = clampWidth(
    preferred.right ?? (viewport <= 1100 ? 360 : 400),
    panelMinimum.right,
    Math.min(640, viewport - left - centerMinimum),
  );
  return { left, right };
}
