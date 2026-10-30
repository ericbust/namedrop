import {
  type ChangeEvent,
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  accept,
  type Candidate,
  type CompletionContext,
  complete,
  composerHint,
  insertion,
  leadingMentions,
  postSource,
} from "../complete.ts";
import {
  composerCopy,
  disabledChannelCopy,
  presenceWord,
  recipientsLabel,
  sendErrorCopy,
} from "../copy.ts";
import type {
  ComposerId,
  Draft,
  DraftKey,
  Outgoing,
  SendRule,
} from "../model.ts";
import { draftKey, inFlight } from "../store.ts";
import { Avatar } from "./Avatar.tsx";
import { Icon } from "./Icons.tsx";
import { measure } from "./measure.ts";

const optionId = (list: string, index: number) => `${list}-${index}`;

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
      className="option"
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

export function Composer({
  context,
  composer,
  drafts,
  outbox,
  placeholder,
  label,
  rule,
  notice = null,
  onEdit,
  onSend,
}: {
  context: CompletionContext;
  composer: ComposerId;
  drafts: ReadonlyMap<DraftKey, Draft>;
  outbox: ReadonlyMap<string, Outgoing>;
  placeholder: string;
  label: string;
  rule: SendRule;
  notice?: ReactNode;
  onEdit: (text: string) => void;
  onSend: (source: string, startedAt: number) => void;
}) {
  const [caret, setCaret] = useState(0);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [highlight, setHighlight] = useState({ token: "", index: 0 });
  const [guarded, setGuarded] = useState(false);
  const [width, setWidth] = useState(0);
  const box = useRef<HTMLTextAreaElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const placedCaret = useRef<number | null>(null);
  const fitted = useRef<string | null>(null);
  const typedAt = useRef<number | null>(null);
  const listId = useId();

  const draft = drafts.get(draftKey(composer));
  const text = draft?.text ?? "";
  const failure = draft?.failure ?? null;
  const sending = inFlight(outbox, composer);
  const leading = leadingMentions(text);
  const source = postSource(text, rule);

  const completion = useMemo(
    () => (sending ? null : complete(text, caret, context)),
    [sending, text, caret, context],
  );
  const candidates = completion?.candidates ?? [];
  const open = focused && !dismissed && candidates.length > 0;
  const token =
    completion === null
      ? ""
      : `${completion.token.kind}:${completion.token.start}:${completion.token.query}`;
  const active = !open
    ? -1
    : highlight.token === token
      ? Math.min(highlight.index, candidates.length - 1)
      : Math.max(
          0,
          candidates.findIndex((candidate) => insertion(candidate) !== null),
        );

  useLayoutEffect(() => {
    if (placedCaret.current === null) return;
    box.current?.setSelectionRange(placedCaret.current, placedCaret.current);
    placedCaret.current = null;
  });
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    // Fitting in the callback resizes the observed box during delivery, a ResizeObserver loop error.
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    const element = box.current;
    if (!element) return;
    // Chromium sizes an empty box to its wrapped placeholder, so the width, placeholder and text decide the height.
    const shown = `${width}\n${element.placeholder}\n${element.value}`;
    if (fitted.current === shown) return;
    fitted.current = shown;
    element.style.height = "auto";
    element.style.height = `${element.scrollHeight}px`;
  });
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
  }, [active, listId]);
  useLayoutEffect(() => {
    const start = typedAt.current;
    typedAt.current = null;
    if (start === null || !open || !list.current) return;
    list.current.getBoundingClientRect();
    measure("namedrop:keystroke-to-popup", start);
  });

  function edit(next: string, at: number) {
    onEdit(next);
    setCaret(at);
    setDismissed(false);
    setGuarded(false);
  }
  function choose(index: number) {
    const candidate = candidates[index];
    if (completion === null || candidate === undefined) return;
    const next = accept(text, completion, candidate);
    if (next === null) return;
    placedCaret.current = next.caret;
    edit(next.text, next.caret);
  }
  function submit(startedAt: number) {
    if (sending) return;
    if (source === null) {
      setGuarded(true);
      return;
    }
    onSend(source, startedAt);
  }
  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    const plain = !event.shiftKey;
    if (open) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const step = event.key === "ArrowDown" ? 1 : -1;
        setHighlight({
          token,
          index: (active + step + candidates.length) % candidates.length,
        });
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setDismissed(true);
        return;
      }
      if ((event.key === "Enter" || event.key === "Tab") && plain) {
        event.preventDefault();
        choose(active);
        return;
      }
    }
    if (event.key === "Enter" && plain) {
      event.preventDefault();
      submit(event.timeStamp);
    }
  }
  function onChange(event: ChangeEvent<HTMLTextAreaElement>) {
    typedAt.current = event.timeStamp;
    edit(event.target.value, event.target.selectionStart);
  }

  return (
    <div className="composer">
      <div className="composer-box">
        {open && (
          <div
            ref={list}
            id={listId}
            role="listbox"
            className="completions"
            aria-label={
              completion?.token.kind === "member" ? "Members" : "Channels"
            }
          >
            {candidates.map((candidate, index) => (
              <Option
                key={
                  candidate.kind === "channel"
                    ? candidate.channel.key
                    : candidate.kind === "member"
                      ? candidate.member.id
                      : "#all"
                }
                candidate={candidate}
                id={optionId(listId, index)}
                active={index === active}
                onChoose={() => choose(index)}
              />
            ))}
          </div>
        )}
        {rule.kind === "thread" &&
          rule.to.length > 0 &&
          leading.length === 0 && (
            <span className="composer-to">{recipientsLabel(rule.to)}</span>
          )}
        <textarea
          ref={box}
          rows={1}
          role="combobox"
          aria-label={label}
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          aria-activedescendant={
            active < 0 ? undefined : optionId(listId, active)
          }
          placeholder={placeholder}
          value={text}
          readOnly={sending}
          onChange={onChange}
          onKeyDown={onKeyDown}
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onFocus={(event) => {
            setFocused(true);
            setCaret(event.currentTarget.selectionStart);
          }}
          onBlur={() => setFocused(false)}
        />
        <button
          type="button"
          className="send"
          aria-label={composerCopy.send}
          disabled={sending || source === null}
          onMouseDown={(event) => event.preventDefault()}
          onClick={(event) => submit(event.timeStamp)}
        >
          <Icon name="arrowUp" />
        </button>
      </div>
      <div className="composer-status">
        {failure !== null ? (
          <p className="composer-error" role="alert">
            <Icon name="alert" className="icon-14" />
            {sendErrorCopy(failure)}
          </p>
        ) : sending ? (
          <p>{composerCopy.sending}</p>
        ) : notice !== null ? (
          notice
        ) : guarded && source === null && text.trim() !== "" ? (
          <p className="composer-guard">
            {leading.length > 0
              ? composerCopy.needsMessage(leading)
              : composerCopy.needsRecipient}
          </p>
        ) : focused ? (
          <p>{composerHint(rule, text)}</p>
        ) : null}
      </div>
    </div>
  );
}
