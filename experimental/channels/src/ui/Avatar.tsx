import { avatarHue } from "../copy.ts";
import type { Activity, MemberKind } from "../model.ts";
import { Icon } from "./Icons.tsx";

export function Avatar({
  alias,
  type,
  small = false,
  presence,
}: {
  alias: string;
  type: MemberKind["type"] | "unresolved";
  small?: boolean;
  presence?: Activity;
}) {
  const tone =
    type === "human"
      ? "id-operator"
      : type === "terminal" || type === "unresolved"
        ? "id-terminal"
        : `id-${avatarHue(alias)}`;
  return (
    <span className={`avatar${small ? " sm" : ""} ${tone}`} aria-hidden="true">
      {type === "terminal" ? (
        <Icon name="prompt" />
      ) : type === "human" ? (
        "Y"
      ) : type === "unresolved" ? (
        "?"
      ) : (
        ([...alias.replace(/^@/, "")][0] ?? "?").toUpperCase()
      )}
      {presence !== undefined && presence !== "retired" && (
        <span className="presence" data-activity={presence} />
      )}
    </span>
  );
}
