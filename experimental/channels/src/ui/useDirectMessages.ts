import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type DirectHistory,
  type DirectPeer,
  deriveDirectPeers,
  directComposer,
  directKey,
  directPath,
  heardDirect,
  mergeDirectHistory,
} from "../direct-messages.ts";
import type { Channel, Draft, DraftKey, Outgoing, SpaceKey } from "../model.ts";
import type { createPoster } from "../send.ts";
import { type Heard, nothingHeard } from "../sounds.ts";
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
function useDirectPoll(
  request: ((signal: AbortSignal) => Promise<void>) | null,
) {
  const retry = useRef(() => {});
  useEffect(() => {
    if (request === null) return;
    let controller: AbortController;
    let timer: ReturnType<typeof setTimeout>;
    const poll = () => {
      controller?.abort();
      clearTimeout(timer);
      controller = new AbortController();
      const { signal } = controller;
      void request(signal).finally(() => {
        if (!signal.aborted) timer = setTimeout(poll, 2000);
      });
    };
    retry.current = poll;
    poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
      retry.current = () => {};
    };
  }, [request]);
  return useCallback(() => retry.current(), []);
}
export function useDirectMessages(
  selected: string | null,
  poster: ReturnType<typeof createPoster>,
  channels: ReadonlyMap<SpaceKey, Channel>,
) {
  const [directoryPeers, setPeers] = useState<readonly DirectPeer[]>([]);
  const peers = useMemo(
    () => deriveDirectPeers(directoryPeers, channels),
    [directoryPeers, channels],
  );
  const [directoryError, setDirectoryError] = useState(false);
  const [heard, setHeard] = useState<Heard>(nothingHeard);
  const lastRead = useRef<readonly DirectPeer[] | null>(null);
  const [histories, setHistories] = useState<
    ReadonlyMap<string, DirectHistory>
  >(new Map());
  const [errors, setErrors] = useState<ReadonlySet<string>>(new Set());
  const [drafts, setDrafts] = useState<ReadonlyMap<DraftKey, Draft>>(new Map());
  const [outbox, setOutbox] = useState<ReadonlyMap<string, Outgoing>>(
    new Map(),
  );
  const [reads, setReads] = useState(savedReads);
  const [olderRequest, setOlderRequest] = useState<{
    key: string;
    controller: AbortController;
  } | null>(null);
  const older = olderRequest !== null && olderRequest.key === selected;
  const sending = useRef(new Set<string>());
  const draftsRef = useRef(drafts);
  draftsRef.current = drafts;
  const selectedPeer =
    peers.find((peer) => directKey(peer) === selected) ?? null;
  const peerRef = useRef(selectedPeer);
  peerRef.current = selectedPeer;
  const refreshDirectory = useDirectPoll(
    useCallback(async (signal: AbortSignal) => {
      try {
        const response = await fetch("/api/direct-messages", {
          credentials: "same-origin",
          signal,
        });
        if (!response.ok) throw new Error("Directory unavailable");
        const next = parseDirectPeers(await response.json());
        if (!signal.aborted) {
          const arrivals = heardDirect(lastRead.current, next);
          lastRead.current = next;
          if (arrivals.length > 0)
            setHeard((heard) => ({ seq: heard.seq + 1, arrivals }));
          setPeers(next);
          setDirectoryError(false);
        }
      } catch {
        if (!signal.aborted) setDirectoryError(true);
      }
    }, []),
  );
  const spaceKey = selectedPeer?.spaceKey;
  const addressId = selectedPeer?.addressId;
  const pollHistory = useCallback(
    async (signal: AbortSignal) => {
      if (
        spaceKey === undefined ||
        addressId === undefined ||
        selected === null
      )
        return;
      try {
        const response = await fetch(
          `${directPath({ spaceKey, addressId })}?limit=100`,
          { credentials: "same-origin", signal },
        );
        if (!response.ok) throw new Error("Conversation unavailable");
        const page = parseDirectHistory(await response.json());
        if (!signal.aborted) {
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
        if (!signal.aborted) setErrors((all) => new Set(all).add(selected));
      }
    },
    [selected, spaceKey, addressId],
  );
  const refreshHistory = useDirectPoll(
    selectedPeer === null ? null : pollHistory,
  );
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
            } catch {}
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
            refreshDirectory();
            refreshHistory();
          }
        });
    },
    [poster, refreshDirectory, refreshHistory],
  );
  useEffect(() => {
    if (olderRequest !== null && olderRequest.key !== selected)
      olderRequest.controller.abort();
    return () => olderRequest?.controller.abort();
  }, [selected, olderRequest]);
  const loadOlder = async () => {
    const peer = peerRef.current;
    const before = history?.messages[0]?.id;
    if (!peer || !before || older) return;
    const key = directKey(peer);
    const controller = new AbortController();
    setOlderRequest({ key, controller });
    try {
      const params = new URLSearchParams({ before, limit: "100" });
      const response = await fetch(`${directPath(peer)}?${params}`, {
        credentials: "same-origin",
        signal: controller.signal,
      });
      if (!response.ok) throw new Error("Conversation unavailable");
      const page = parseDirectHistory(await response.json());
      if (!controller.signal.aborted)
        setHistories((all) => {
          const current = all.get(key);
          if (current?.messages[0]?.id !== before) return all;
          return new Map(all).set(
            key,
            mergeDirectHistory(current, page, "prepend"),
          );
        });
    } catch {
      if (!controller.signal.aborted) setErrors((all) => new Set(all).add(key));
    } finally {
      setOlderRequest((current) =>
        current?.controller === controller ? null : current,
      );
    }
  };
  return {
    fullBody: poster.fullBody,
    peers,
    selectedPeer,
    directoryError,
    heard,
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
    retry: () => {
      refreshDirectory();
      refreshHistory();
    },
    pending: [...outbox.values()].filter(
      (entry) =>
        selectedPeer !== null &&
        sameComposer(entry.composer, directComposer(selectedPeer)),
    ),
  };
}
export type DirectMessages = ReturnType<typeof useDirectMessages>;
