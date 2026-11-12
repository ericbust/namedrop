declare module "*.css";

/** Where the build put the separately built diagram code. */
declare const __DIAGRAM_ENTRY__: string;

interface ImportMeta {
  /** Set by Vite: true while `vite dev` serves the source. */
  readonly env: { readonly DEV: boolean };
}
