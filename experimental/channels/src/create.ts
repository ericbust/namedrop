import { createCopy } from "./copy.ts";
import { foldLabel } from "./label.ts";
import type { AddressId, SpaceKey } from "./model.ts";

export type AgentKind = "claude" | "codex" | "terminal";
export const agentKinds: readonly AgentKind[] = ["claude", "codex", "terminal"];

export type ChannelDraft = { name: string; folder: string };
export type AgentDraft = { kind: AgentKind; name: string; persona: string };
export type ChannelRequest = { name: string; folder: string };
export type AgentRequest = {
  kind: AgentKind;
  name: string;
  persona: string | null;
};
export type Checked<T> =
  | { kind: "ok"; request: T }
  | { kind: "problem"; message: string };

export type CreatedChannel = {
  key: SpaceKey;
  workspaceId: string;
  label: string;
};
export type AddedAgent = { addressId: AddressId; alias: string };
export type Refusal = { kind: "refused"; status: number; message: string };
export type ChannelResult =
  | { kind: "created"; channel: CreatedChannel }
  | Refusal;
export type AgentResult = { kind: "created"; agent: AddedAgent } | Refusal;

// Both forms share one machine: a press checks the draft first and waits on
// the server only when the draft passes.
export type FormState =
  | { kind: "editing" }
  | { kind: "pending" }
  | { kind: "failed"; message: string };
export type FormEvent =
  | { type: "submit"; problem: string | null }
  | { type: "refused"; message: string }
  | { type: "edit" };
export const editing: FormState = { kind: "editing" };

export function formState(state: FormState, event: FormEvent): FormState {
  switch (event.type) {
    case "submit":
      if (state.kind === "pending") return state;
      return event.problem === null
        ? { kind: "pending" }
        : { kind: "failed", message: event.problem };
    case "refused":
      return state.kind === "pending"
        ? { kind: "failed", message: event.message }
        : state;
    case "edit":
      return state.kind === "failed" ? editing : state;
    default: {
      const exhaustive: never = event;
      return exhaustive;
    }
  }
}

const MAX_CHANNEL_NAME = 64;
const MAX_PERSONA = 1_000;
// domain::is_invisible_format in src/domain.rs, and Rust's char::is_control.
const INVISIBLE = /[­؜᠎​‎‏‪-‮⁠-⁤⁦-⁯﻿￹-￻\u{E0000}-\u{E007F}]/u;
const CONTROL = /\p{Cc}/u;
const ALIAS = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

// A label the server can route to: it has a letter or a digit, and isn't the
// word that means every workspace.
function routable(label: string): boolean {
  return /[a-z0-9]/.test(label) && label !== "all";
}
export function channelRequest(draft: ChannelDraft): Checked<ChannelRequest> {
  const name = draft.name.trim();
  const folder = draft.folder.trim();
  const problem =
    name === ""
      ? createCopy.channelNameMissing
      : [...name].length > MAX_CHANNEL_NAME
        ? createCopy.channelNameLong
        : CONTROL.test(name) || INVISIBLE.test(name)
          ? createCopy.channelNameHidden
          : !routable(foldLabel(name))
            ? createCopy.channelNameUnusable
            : !folder.startsWith("/") && !folder.startsWith("~")
              ? createCopy.folderNotPath
              : null;
  return problem === null
    ? { kind: "ok", request: { name, folder } }
    : { kind: "problem", message: problem };
}

export function bareName(name: string): string {
  return name.trim().replace(/^@/, "");
}
// The check shown as the person types: only what more typing can't fix, so a
// name on its way to "code-reviewer" never reads as wrong at "code-".
export function liveNameProblem(
  name: string,
  taken: ReadonlySet<string>,
): string | null {
  const bare = bareName(name);
  if (bare === "") return null;
  if (/[^a-z0-9-]/.test(bare) || bare.startsWith("-") || bare.includes("--"))
    return createCopy.agentNameRule;
  return taken.has(bare) ? createCopy.agentNameTaken(bare) : null;
}
export function agentRequest(
  draft: AgentDraft,
  taken: ReadonlySet<string>,
): Checked<AgentRequest> {
  const name = bareName(draft.name);
  const persona = draft.persona.trim();
  const problem = !ALIAS.test(name)
    ? createCopy.agentNameRule
    : taken.has(name)
      ? createCopy.agentNameTaken(name)
      : [...persona].length > MAX_PERSONA
        ? createCopy.personaLong
        : CONTROL.test(persona) || INVISIBLE.test(persona)
          ? createCopy.personaInvisible
          : null;
  return problem === null
    ? {
        kind: "ok",
        request: { kind: draft.kind, name, persona: persona || null },
      }
    : { kind: "problem", message: problem };
}

// The names a new agent can't take: every member the channel lists, closed
// ones too, since the server keeps a retired name.
export function takenNames(
  members: Iterable<{ alias: string }>,
): ReadonlySet<string> {
  return new Set([...members].map((member) => member.alias.replace(/^@/, "")));
}
