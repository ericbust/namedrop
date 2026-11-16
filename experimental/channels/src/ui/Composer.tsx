import {
  type ChangeEvent,
  type ClipboardEvent,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  accept,
  type CompletionContext,
  complete,
  composerHint,
  insertion,
  keptHint,
  leadingMentions,
  postSource,
  sentText,
  skillRecipients,
  skillToken,
} from "../complete.ts";
import { composerCopy, recipientsLabel, sendErrorCopy } from "../copy.ts";
import {
  type AttachedImage,
  draftImages,
  IMAGE_TYPES,
  type ImageRefusal,
  imageFiles,
  imageUrl,
  refusalOf,
  withImages,
} from "../images.ts";
import type {
  ComposerId,
  Draft,
  DraftKey,
  Outgoing,
  SendRule,
} from "../model.ts";
import { draftKey, inFlight } from "../store.ts";
import { CompletionList, optionId } from "./CompletionList.tsx";
import { Icon } from "./Icons.tsx";
import { ImageUploadContext } from "./ImageUploadContext.ts";
import { measure } from "./measure.ts";
import { useSkills } from "./useSkills.ts";

type Uploading = { id: number; at: DraftKey };
type Refused = { at: DraftKey; why: ImageRefusal };

export function Composer({
  context,
  composer,
  drafts,
  outbox,
  placeholder,
  label,
  rule,
  notice = null,
  closed = null,
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
  // Why nothing can be sent from here, such as a workspace with no agents.
  closed?: string | null;
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
  const upload = useContext(ImageUploadContext);
  // Uploads still running, each with the draft it started in.
  const [uploads, setUploads] = useState<readonly Uploading[]>([]);
  const running = useRef<readonly Uploading[]>([]);
  const [refused, setRefused] = useState<Refused | null>(null);
  const [dragging, setDragging] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const uploadId = useRef(0);

  const at = draftKey(composer);
  const draft = drafts.get(at);
  // A draft's text ends with a line for each image attached to it.
  const { text, images } = useMemo(
    () => draftImages(draft?.text ?? ""),
    [draft?.text],
  );
  // What an upload that finishes later adds its image to.
  const shown = useRef({ at, text, images, onEdit });
  useLayoutEffect(() => {
    shown.current = { at, text, images, onEdit };
  });
  const failure = draft?.failure ?? null;
  const sending = inFlight(outbox, composer);
  const leading = leadingMentions(sentText(text));
  const words = postSource(text, rule);
  const source = words === null ? null : withImages(words, images);
  const uploading = uploads.filter((upload) => upload.at === at);
  const attaching = uploading.length > 0;
  const refusal = refused?.at === at ? refused.why : null;
  const missingRecipient =
    rule.kind === "thread" && rule.to.length === 0 && leading.length === 0;
  const pickRecipient =
    rule.kind === "thread" &&
    rule.to.length > 1 &&
    leading.length === 0 &&
    (guarded || sentText(text) !== "");

  const skillTargets =
    skillToken(text, caret) && !sending ? skillRecipients(text, context) : [];
  const catalogs = useSkills(skillTargets);
  const completion = useMemo(
    () => (sending ? null : complete(text, caret, context, catalogs)),
    [sending, text, caret, context, catalogs],
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
    const start = typedAt.current;
    typedAt.current = null;
    if (start === null || !open || !list.current) return;
    list.current.getBoundingClientRect();
    measure("namedrop:keystroke-to-popup", start);
  });

  function edit(next: string, caretAt: number) {
    onEdit(withImages(next, images));
    setCaret(caretAt);
    setDismissed(false);
    setGuarded(false);
    setRefused(null);
  }
  function track(next: readonly Uploading[]) {
    running.current = next;
    setUploads(next);
  }
  function attach(files: readonly File[]) {
    if (upload === null || closed !== null || sending) return;
    setRefused(null);
    for (const file of files) {
      const why = refusalOf(
        file,
        shown.current.images.length +
          running.current.filter((other) => other.at === at).length,
      );
      if (why !== null) {
        setRefused({ at, why });
        continue;
      }
      uploadId.current += 1;
      const id = uploadId.current;
      track([...running.current, { id, at }]);
      void upload(file).then((result) => {
        track(running.current.filter((other) => other.id !== id));
        if (result.kind === "refused") {
          setRefused({ at, why: result.why });
          return;
        }
        const now = shown.current;
        // The person moved to another conversation while it uploaded.
        if (now.at !== at) return;
        if (now.images.some((image) => image.name === result.image.name))
          return;
        const added = [...now.images, result.image];
        shown.current = { ...now, images: added };
        now.onEdit(withImages(now.text, added));
      });
    }
  }
  function remove(image: AttachedImage) {
    onEdit(
      withImages(
        text,
        images.filter((other) => other !== image),
      ),
    );
    setRefused(null);
    box.current?.focus();
  }
  function onPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (upload === null) return;
    const files = imageFiles(event.clipboardData, "paste");
    if (files.length === 0) return;
    event.preventDefault();
    attach(files);
  }
  function carriesFiles(event: DragEvent<HTMLDivElement>): boolean {
    return upload !== null && event.dataTransfer.types.includes("Files");
  }
  function onDragOver(event: DragEvent<HTMLDivElement>) {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    setDragging(true);
  }
  function onDrop(event: DragEvent<HTMLDivElement>) {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    setDragging(false);
    const files = imageFiles(event.dataTransfer, "drop");
    if (files.length === 0) setRefused({ at, why: "type" });
    else attach(files);
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
    if (sending || attaching) return;
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

  const attachments = images.length > 0 || attaching;
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: a drop target; the attach button does the same from the keyboard.
    <div
      className={dragging ? "composer dragging" : "composer"}
      onDragEnter={onDragOver}
      onDragOver={onDragOver}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null))
          setDragging(false);
      }}
      onDrop={onDrop}
    >
      <div className={attachments ? "composer-box has-images" : "composer-box"}>
        {attachments && (
          <ul
            className="composer-images"
            aria-label={composerCopy.attachedImages}
          >
            {images.map((image, index) => (
              <li key={image.name} className="image-chip">
                <img
                  src={imageUrl(image.name)}
                  alt={composerCopy.attachedImage(index + 1)}
                />
                <button
                  type="button"
                  className="image-remove"
                  aria-label={composerCopy.removeImage(index + 1)}
                  disabled={sending}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => remove(image)}
                >
                  <Icon name="x" className="icon-12" />
                </button>
              </li>
            ))}
            {uploading.map(({ id }) => (
              <li key={id} className="image-chip uploading">
                <span className="sr-only" role="status">
                  {composerCopy.uploadingImage}
                </span>
              </li>
            ))}
          </ul>
        )}
        {open && completion !== null && (
          <CompletionList
            completion={completion}
            active={active}
            listId={listId}
            list={list}
            onChoose={choose}
          />
        )}
        {rule.kind === "thread" &&
          rule.implicitRecipient !== null &&
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
          disabled={closed !== null}
          onChange={onChange}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onFocus={(event) => {
            setFocused(true);
            setCaret(event.currentTarget.selectionStart);
          }}
          onBlur={() => setFocused(false)}
        />
        {upload !== null && (
          <>
            <input
              ref={picker}
              type="file"
              hidden
              multiple
              accept={IMAGE_TYPES.join(",")}
              onChange={(event) => {
                attach([...(event.target.files ?? [])]);
                event.target.value = "";
              }}
            />
            <button
              type="button"
              className="attach"
              aria-label={composerCopy.attachImage}
              title={composerCopy.attachImage}
              disabled={closed !== null || sending}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => picker.current?.click()}
            >
              <Icon name="image" />
            </button>
          </>
        )}
        <button
          type="button"
          className="send"
          aria-label={composerCopy.send}
          disabled={closed !== null || sending || attaching || source === null}
          onMouseDown={(event) => event.preventDefault()}
          onClick={(event) => submit(event.timeStamp)}
        >
          <Icon name="arrowUp" />
        </button>
      </div>
      <div className="composer-status">
        {closed !== null ? (
          <p>{closed}</p>
        ) : failure !== null ? (
          <p className="composer-error" role="alert">
            <Icon name="alert" className="icon-14" />
            {sendErrorCopy(failure)}
          </p>
        ) : sending ? (
          <p>{composerCopy.sending}</p>
        ) : refusal !== null ? (
          <p className="composer-guard" role="status">
            {composerCopy.imageRefused[refusal]}
          </p>
        ) : dragging ? (
          <p>{composerCopy.dropImages}</p>
        ) : attaching ? (
          <p>{composerCopy.uploadingImage}</p>
        ) : images.length > 0 && sentText(text) === "" ? (
          <p className="composer-guard">{composerCopy.needsWords}</p>
        ) : notice !== null ? (
          notice
        ) : pickRecipient ? (
          <p>
            {sentText(text) === "" && rule.kind === "thread"
              ? composerCopy.pickRecipient(rule.to)
              : composerHint(rule, text)}
          </p>
        ) : (guarded || (focused && missingRecipient)) &&
          source === null &&
          (missingRecipient || sentText(text) !== "") ? (
          <p className="composer-guard">
            {missingRecipient ? composerCopy.needsRecipient : keptHint(text)}
          </p>
        ) : focused ? (
          <p>{composerHint(rule, text)}</p>
        ) : null}
      </div>
    </div>
  );
}
