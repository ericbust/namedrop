import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createCopy } from "../copy.ts";
import {
  type ChannelDraft,
  type ChannelRequest,
  type ChannelResult,
  channelRequest,
} from "../create.ts";
import type { SpaceKey } from "../model.ts";
import { CreateForm, useCreateForm } from "./CreateForm.tsx";

export type ChannelCreator = {
  folders: (key: SpaceKey, signal: AbortSignal) => Promise<readonly string[]>;
  createChannel: (
    key: SpaceKey,
    request: ChannelRequest,
  ) => Promise<ChannelResult>;
};

// `target` is the channel whose Herdr session gets the new workspace.
export function NewChannelForm({
  id,
  target,
  creator,
  onCreated,
  onClose,
}: {
  id: string;
  target: SpaceKey;
  creator: ChannelCreator;
  onCreated: (key: SpaceKey) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<ChannelDraft>({ name: "", folder: "" });
  const [recent, setRecent] = useState<readonly string[]>([]);
  const { state, submit, edit } = useCreateForm();
  const nameField = useRef<HTMLInputElement>(null);
  const folderField = useRef<HTMLInputElement>(null);
  const recentId = useId();
  useLayoutEffect(() => {
    nameField.current?.focus();
    nameField.current?.scrollIntoView({ block: "nearest" });
  }, []);
  useEffect(() => {
    const request = new AbortController();
    void creator.folders(target, request.signal).then((folders) => {
      if (!request.signal.aborted) setRecent(folders);
    });
    return () => request.abort();
  }, [creator, target]);
  function change(next: Partial<ChannelDraft>) {
    setDraft((now) => ({ ...now, ...next }));
    edit();
  }
  return (
    <CreateForm
      id={id}
      label={createCopy.newChannel}
      className="new-channel"
      state={state}
      message={state.kind === "failed" ? state.message : null}
      submitLabel={createCopy.create}
      pendingLabel={createCopy.creating}
      onCancel={onClose}
      onSubmit={() =>
        submit(channelRequest(draft), async (request) => {
          const result = await creator.createChannel(target, request);
          if (result.kind !== "created") return result;
          onCreated(result.channel.key);
          return null;
        })
      }
    >
      <label className="create-field">
        <span className="create-label">{createCopy.name}</span>
        <input
          ref={nameField}
          type="text"
          autoComplete="off"
          value={draft.name}
          onChange={(event) => change({ name: event.target.value })}
        />
      </label>
      <label className="create-field">
        <span className="create-label">{createCopy.folder}</span>
        <input
          ref={folderField}
          type="text"
          autoComplete="off"
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          enterKeyHint="go"
          placeholder={createCopy.folderPlaceholder}
          value={draft.folder}
          onChange={(event) => change({ folder: event.target.value })}
        />
      </label>
      {recent.length > 0 && (
        <div className="folder-choices">
          <p className="create-label" id={recentId}>
            {createCopy.recentFolders}
          </p>
          <ul className="folder-list" aria-labelledby={recentId}>
            {recent.map((folder) => (
              <li key={folder}>
                <button
                  type="button"
                  className="folder-choice"
                  title={folder}
                  data-chosen={draft.folder.trim() === folder || undefined}
                  onClick={() => {
                    change({ folder });
                    // The field shows the end of the path, where its own name is.
                    requestAnimationFrame(() => {
                      const field = folderField.current;
                      if (field) field.scrollLeft = field.scrollWidth;
                    });
                  }}
                >
                  <bdi>{folder}</bdi>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </CreateForm>
  );
}
