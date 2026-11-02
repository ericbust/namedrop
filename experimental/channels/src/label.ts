// Namedrop's own fold of a Herdr label, WorkspaceLabel::from_herdr in src/spaces.rs.
// The Rust trims first, which changes nothing here: a run outside the token set at
// either end never emits a dash.
const TOKEN_CHAR = /^[A-Za-z0-9._-]$/;
const MAX_LABEL_CHARS = 64;

export function foldLabel(label: string): string {
  let folded = "";
  let separator = false;
  for (const character of label) {
    if (TOKEN_CHAR.test(character)) {
      if (separator && folded !== "") folded += "-";
      separator = false;
      folded += character.toLowerCase();
    } else separator = true;
  }
  return folded.slice(0, MAX_LABEL_CHARS);
}
