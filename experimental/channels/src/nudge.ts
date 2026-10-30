// Server nudges name tracked work by id (src/store/coordination.rs). The page
// shows plain words and keeps the original text for the tooltip.
const ID = String.raw`(?<![\w-])(?:coordination|assignment|message|thread|delivery-attempt|delivery|attempt|request)-\d+-\d+-\d+(?![\w-])`;
const COORDINATION = new RegExp(
  String.raw`\bCoordination[ \t]+(?=coordination-)${ID}`,
  "g",
);
const ASSIGNMENT = new RegExp(
  String.raw`\bAssignment[ \t]+(?=assignment-)(${ID})(?:[ \t]+revision[ \t]+(\d+))?(?::[ \t]*([a-z]+(?:_[a-z]+)*))?`,
  "g",
);
const BARE = new RegExp(String.raw`([ \t]*)${ID}([ \t]*)`, "g");

export function plainNudge(text: string): string {
  const numbers = new Map<string, number>();
  return text
    .replace(COORDINATION, "This coordination")
    .replace(
      ASSIGNMENT,
      (_, id: string, revision?: string, outcome?: string) => {
        const number = numbers.get(id) ?? numbers.size + 1;
        numbers.set(id, number);
        return `Assignment ${number}${revision ? `, revision ${revision}` : ""}${outcome ? `: ${outcome.replaceAll("_", " ")}` : ""}`;
      },
    )
    .replace(BARE, (_, before: string, after: string) =>
      before && after ? " " : "",
    );
}
