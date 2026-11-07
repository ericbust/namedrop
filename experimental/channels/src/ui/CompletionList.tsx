import { type ReactNode, type RefObject, useLayoutEffect } from "react";
import { type Candidate, type Completion, insertion } from "../complete.ts";
import { composerCopy, disabledChannelCopy, presenceWord } from "../copy.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";

export const optionId = (list: string, index: number) => `${list}-${index}`;

function Option({
  candidate,
  id,
  active,
  onChoose,
}: {
  candidate: Candidate;
  id: string;
  active: boolean;
  onChoose: () => void;
}) {
  const disabled = insertion(candidate) === null;
  let content: ReactNode;
  let title: string | undefined;
  switch (candidate.kind) {
    case "channel": {
      const { channel } = candidate;
      title =
        channel.disabledReason === null
          ? undefined
          : disabledChannelCopy[channel.disabledReason];
      content = (
        <>
          <Icon name="hash" className="opt-icon" />
          <span className="opt-name">
            {channel.name}
            {channel.suffix !== null && (
              <span className="row-suffix"> · {channel.suffix}</span>
            )}
          </span>
          {channel.token !== null && (
            <span className="opt-meta">{channel.token}</span>
          )}
        </>
      );
      break;
    }
    case "all":
      content = (
        <>
          <Icon name="hash" className="opt-icon" />
          <span className="opt-name">#all</span>
          <span className="opt-meta">· {composerCopy.everyWorkspace}</span>
        </>
      );
      break;
    case "skill": {
      const { skill, runtime, badge } = candidate;
      content = (
        <>
          <span className="opt-skill-copy">
            <span className="opt-name">{insertion(candidate)}</span>
            <span className="opt-description">{skill.description}</span>
          </span>
          <span className="opt-skill-meta">
            {badge && <span>{runtime === "claude" ? "Claude" : "Codex"}</span>}
            <span>{skill.source}</span>
          </span>
        </>
      );
      break;
    }
    case "member": {
      const { member } = candidate;
      content = (
        <>
          <Avatar
            alias={member.alias}
            type={member.kind.type}
            small
            presence={member.activity}
          />
          <span className="opt-name">{member.alias}</span>
          <span className="opt-presence">{presenceWord[member.activity]}</span>
        </>
      );
      break;
    }
    default: {
      const exhaustive: never = candidate;
      return exhaustive;
    }
  }
  return (
    // biome-ignore lint/a11y/useKeyWithClickEvents: keys reach options through the combobox's aria-activedescendant.
    <div
      role="option"
      id={id}
      className={`option${candidate.kind === "skill" ? " option-skill" : ""}`}
      aria-selected={active}
      aria-disabled={disabled || undefined}
      title={title}
      tabIndex={-1}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onChoose}
    >
      {content}
    </div>
  );
}

export function CompletionList({
  completion,
  active,
  listId,
  list,
  onChoose,
}: {
  completion: Completion;
  active: number;
  listId: string;
  list: RefObject<HTMLDivElement | null>;
  onChoose: (index: number) => void;
}) {
  useLayoutEffect(() => {
    const element = list.current;
    const option =
      active < 0 ? null : document.getElementById(optionId(listId, active));
    if (!element || !option) return;
    if (option.offsetTop < element.scrollTop)
      element.scrollTop = option.offsetTop;
    else if (
      option.offsetTop + option.offsetHeight >
      element.scrollTop + element.clientHeight
    )
      element.scrollTop =
        option.offsetTop + option.offsetHeight - element.clientHeight;
  });
  return (
    <div
      ref={list}
      id={listId}
      role="listbox"
      className="completions"
      aria-label={
        completion.token.kind === "skill"
          ? "Skills"
          : completion.token.kind === "member"
            ? "Members"
            : "Channels"
      }
    >
      {completion.candidates.map((candidate, index) => (
        <Option
          key={
            candidate.kind === "skill"
              ? `${candidate.runtime}:${candidate.skill.name}`
              : candidate.kind === "channel"
                ? candidate.channel.key
                : candidate.kind === "member"
                  ? candidate.member.id
                  : "#all"
          }
          candidate={candidate}
          id={optionId(listId, index)}
          active={index === active}
          onChoose={() => onChoose(index)}
        />
      ))}
    </div>
  );
}
