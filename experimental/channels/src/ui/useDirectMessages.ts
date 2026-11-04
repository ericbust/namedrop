import { useCallback, useEffect, useRef, useState } from "react";
import {
  type DirectHistory,
  type DirectPeer,
  directComposer,
  directKey,
  directPath,
  mergeDirectHistory,
} from "../direct-messages.ts";
import type { Draft, DraftKey, Outgoing } from "../model.ts";
import { createPoster } from "../send.ts";
import { draftKey, sameComposer } from "../store.ts";
import { parseDirectHistory, parseDirectPeers } from "../wire.ts";

const READ_KEY = "namedrop:dm-read:v1";
function savedReads(): Map<string, string> {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(READ_KEY) ?? "{}");
    if (typeof raw !== "object" || raw === null) return new Map();
    return new Map(
      Object.entries(raw).filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      ),
    );
  } catch {
    return new Map();
  }
}
export function useDirectMessages(selected: string | null) {
  const [peers, setPeers] = useState<readonly DirectPeer[]>([]);
  const [directoryError, setDirectoryError] = useState(false);
  const [histories, setHistories] = useState<
    ReadonlyMap<string, DirectHistory>
  >(new Map());
  const [errors, setErrors] = useState<ReadonlySet<string>>(new Set());
  const [drafts, setDrafts] = useState<ReadonlyMap<DraftKey, Draft>>(new Map());
  const [outbox, setOutbox] = useState<ReadonlyMap<string, Outgoing>>(
    new Map(),
  );
  const [reads, setReads] = useState(savedReads);
  const [revision, refresh] = useState(0);
  const [older, setOlder] = useState(false);
  const [poster] = useState(createPoster);
  const sending = useRef(new Set<string>());
  const draftsRef = useRef(drafts);
  draftsRef.current = drafts;
  const selectedPeer =
    peers.find((peer) => directKey(peer) === selected) ?? null;
  const peerRef = useRef(selectedPeer);
  peerRef.current = selectedPeer;
  useEffect(() => poster.prepare(), [poster]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: A retry restarts the independent directory poll.
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const response = await fetch("/api/direct-messages", {
          credentials: "same-origin",
          signal: controller.signal,
        });
        if (!response.ok) throw new Error("Directory unavailable");
        const next = parseDirectPeers(await response.json());
        if (!controller.signal.aborted) {
          setPeers(next);
          setDirectoryError(false);
        }
      } catch {
        if (!controller.signal.aborted) setDirectoryError(true);
      }
      if (!controller.signal.aborted)
        timer = setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [revision]);
  const spaceKey = selectedPeer?.spaceKey;
  const addressId = selectedPeer?.addressId;
  // biome-ignore lint/correctness/useExhaustiveDependencies: A retry refreshes the selected private history.
  useEffect(() => {
    if (spaceKey === undefined || addressId === undefined || selected === null)
      return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const response = await fetch(
          `${directPath({ spaceKey, addressId })}?limit=100`,
          { credentials: "same-origin", signal: controller.signal },
        );
        if (!response.ok) throw new Error("Conversation unavailable");
        const page = parseDirectHistory(await response.json());
        if (!controller.signal.aborted) {
          setHistories((all) =>
            new Map(all).set(
              selected,
              mergeDirectHistory(all.get(selected), page),
            ),
          );
          setErrors((all) => {
            const next = new Set(all);
            next.delete(selected);
            return next;
          });
          const ids = new Set(page.messages.map((message) => message.id));
          setOutbox(
            (all) =>
              new Map(
                [...all].filter(
                  ([, entry]) =>
                    entry.phase.kind !== "sent" ||
                    !ids.has(entry.phase.messageId),
                ),
              ),
          );
        }
      } catch {
        if (!controller.signal.aborted)
          setErrors((all) => new Set(all).add(selected));
      }
      if (!controller.signal.aborted)
        timer = setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [selected, spaceKey, addressId, revision]);
  const history = selected === null ? undefined : histories.get(selected);
  const lastAgent = selectedPeer?.lastAgentMessageId;
  useEffect(() => {
    if (
      selected === null ||
      !history ||
      !lastAgent ||
      !history.messages.some((message) => message.id === lastAgent)
    )
      return;
    let timer: ReturnType<typeof setTimeout>;
    const read = () => {
      clearTimeout(timer);
      if (document.hidden) return;
      timer = setTimeout(
        () =>
          setReads((all) => {
            if (all.get(selected) === lastAgent) return all;
            const next = new Map(all).set(selected, lastAgent);
            try {
              localStorage.setItem(
                READ_KEY,
                JSON.stringify(Object.fromEntries(next)),
              );
            } catch {
              /* Storage may be unavailable. */
            }
            return next;
          }),
        1500,
      );
    };
    read();
    document.addEventListener("visibilitychange", read);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", read);
    };
  }, [selected, lastAgent, history]);
  const edit = useCallback((peer: DirectPeer, text: string) => {
    const composer = directComposer(peer);
    setDrafts((all) => {
      const old = all.get(draftKey(composer));
      return new Map(all).set(draftKey(composer), {
        ...composer,
        text,
        attempt: old?.attempt ?? null,
        failure: null,
      });
    });
  }, []);
  const send = useCallback(
    (peer: DirectPeer, source: string) => {
      const key = directKey(peer);
      if (peer.closed || sending.current.has(key)) return;
      sending.current.add(key);
      const composer = directComposer(peer);
      const tried = draftsRef.current.get(draftKey(composer))?.attempt;
      const requestId =
        tried?.source === source ? tried.requestId : crypto.randomUUID();
      const attempt = { requestId, source, inReplyTo: null };
      setDrafts((all) =>
        new Map(all).set(draftKey(composer), {
          ...composer,
          text: source,
          attempt,
          failure: null,
        }),
      );
      setOutbox((all) =>
        new Map(all).set(requestId, {
          requestId,
          composer,
          source,
          inReplyTo: null,
          at: Date.now(),
          phase: { kind: "sending" },
        }),
      );
      void poster
        .post({
          key: peer.spaceKey,
          directTo: peer.addressId,
          requestId,
          source,
          inReplyTo: null,
        })
        .then((result) => {
          sending.current.delete(key);
          if (result.kind === "failed") {
            setOutbox((all) => {
              const next = new Map(all);
              next.delete(requestId);
              return next;
            });
            setDrafts((all) =>
              new Map(all).set(draftKey(composer), {
                ...composer,
                text: source,
                attempt,
                failure: result,
              }),
            );
          } else {
            setDrafts((all) => {
              const next = new Map(all);
              next.delete(draftKey(composer));
              return next;
            });
            setOutbox((all) => {
              const entry = all.get(requestId);
              return entry
                ? new Map(all).set(requestId, {
                    ...entry,
                    phase: { kind: "sent", messageId: result.messageId },
                  })
                : all;
            });
            refresh((value) => value + 1);
          }
        });
    },
    [poster],
  );
  const olderRequest = useRef<AbortController | null>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Navigation cancels pagination even while its peer remains in the directory.
  useEffect(() => {
    setOlder(false);
    return () => olderRequest.current?.abort();
  }, [selected]);
  const loadOlder = async () => {
    const peer = peerRef.current;
    const before = history?.messages[0]?.id;
    if (!peer || !before || older) return;
    const key = directKey(peer);
    const controller = new AbortController();
    olderRequest.current = controller;
    setOlder(true);
    try {
      const params = new URLSearchParams({ before, limit: "100" });
      const response = await fetch(`${directPath(peer)}?${params}`, {
        credentials: "same-origin",
        signal: controller.signal,
      });
      if (!response.ok) throw new Error("Conversation unavailable");
      const page = parseDirectHistory(await response.json());
      if (!controller.signal.aborted)
        setHistories((all) =>
          new Map(all).set(key, mergeDirectHistory(all.get(key), page)),
        );
    } catch {
      if (!controller.signal.aborted) setErrors((all) => new Set(all).add(key));
    } finally {
      if (!controller.signal.aborted) setOlder(false);
    }
  };
  return {
    peers,
    selectedPeer,
    directoryError,
    history,
    drafts,
    outbox,
    older,
    historyError: selected !== null && errors.has(selected),
    unread: new Set(
      peers
        .filter(
          (peer) =>
            peer.lastAgentMessageId !== null &&
            reads.get(directKey(peer)) !== peer.lastAgentMessageId,
        )
        .map(directKey),
    ),
    edit,
    send,
    loadOlder,
    retry: () => refresh((value) => value + 1),
    pending: [...outbox.values()].filter(
      (entry) =>
        selectedPeer !== null &&
        sameComposer(entry.composer, directComposer(selectedPeer)),
    ),
  };
}
export type DirectMessages = ReturnType<typeof useDirectMessages>;
