import type { AddressId, SpaceKey } from "./model.ts";

export type SkillRuntime = "claude" | "codex";
export type Skill = {
  name: string;
  description: string;
  source: "personal" | "project" | "plugin";
};
export type SkillTarget = { key: SpaceKey; member: AddressId };
export type SkillCatalog = SkillTarget & {
  runtime: SkillRuntime | null;
  skills: readonly Skill[];
};

export function skillTargetKey(target: SkillTarget): string {
  return `${target.key}/${target.member}`;
}

// The catalog is file content crossing the HTTP boundary, including its names.
export function parseSkills(
  value: unknown,
): Pick<SkillCatalog, "runtime" | "skills"> | null {
  if (
    typeof value !== "object" ||
    value === null ||
    !("runtime" in value) ||
    !("skills" in value) ||
    (value.runtime !== "claude" &&
      value.runtime !== "codex" &&
      value.runtime !== null) ||
    !Array.isArray(value.skills)
  )
    return null;
  const skills: Skill[] = [];
  for (const entry of value.skills.slice(0, 1000)) {
    if (
      typeof entry !== "object" ||
      entry === null ||
      !("name" in entry) ||
      typeof entry.name !== "string" ||
      entry.name.length > 200 ||
      !/^[A-Za-z0-9][A-Za-z0-9:._-]*$/.test(entry.name) ||
      !("description" in entry) ||
      typeof entry.description !== "string" ||
      !("source" in entry) ||
      (entry.source !== "personal" &&
        entry.source !== "project" &&
        entry.source !== "plugin")
    )
      continue;
    skills.push({
      name: entry.name,
      description: entry.description.replace(/\s+/g, " ").slice(0, 240),
      source: entry.source,
    });
  }
  return { runtime: value.runtime, skills };
}
