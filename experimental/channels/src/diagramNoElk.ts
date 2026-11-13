// Stands in for the ELK layout engine in the diagram build. Layout is locked
// to dagre (see diagramConfig.ts), so Mermaid never asks for ELK, and its
// 1.4 MB stay out of the binary. If layout is ever unlocked, a diagram that
// asks for ELK fails to draw and shows its source.
export default class {
  constructor() {
    throw new Error("the ELK layout engine is not built into Channels");
  }
}
