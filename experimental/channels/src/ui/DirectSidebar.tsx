import { useState } from "react";
import { presenceWord, runtimeName } from "../copy.ts";
import {
  type DirectPeer,
  directKey,
  directLabel,
  directPeers,
} from "../direct-messages.ts";
import type { DirectMessages } from "./useDirectMessages.ts";

export function DirectRuntime({ runtime }: { runtime: string }) {
  return (
    <svg
      className="icon dm-runtime"
      viewBox="0 0 16 16"
      role="img"
      aria-label={runtimeName(runtime)}
    >
      {runtime.toLowerCase().includes("codex") ? (
        <path d="m5 3-4 5 4 5M11 3l4 5-4 5M9 2 7 14" />
      ) : (
        <path d="M8 1v14M1 8h14M3 3l10 10M3 13 13 3" />
      )}
    </svg>
  );
}
export function DirectSidebar({
  dm,
  selected,
  onSelect,
}: {
  dm: DirectMessages;
  selected: string | null;
  onSelect(peer: DirectPeer): void;
}) {
  const [filter, setFilter] = useState("");
  const [closed, setClosed] = useState(false);
  const peers = directPeers(dm.peers, filter);
  const active = peers.filter((peer) => !peer.closed);
  const retired = peers.filter((peer) => peer.closed);
  function row(peer: DirectPeer) {
    const key = directKey(peer);
    const unread = dm.unread.has(key);
    const label = directLabel(peer);
    return (
      <li key={key}>
        <button
          type="button"
          className={`row dm-row${unread ? " unread" : ""}`}
          data-dm-key={key}
          title={`${label}${peer.paneLabel?.text ? ` · ${peer.paneLabel.text}` : ""} · ${presenceWord[peer.activity]}`}
          aria-label={`${label}${peer.paneLabel?.text ? ` · ${peer.paneLabel.text}` : ""} · ${presenceWord[peer.activity]}`}
          aria-current={selected === key ? "page" : undefined}
          onClick={() => onSelect(peer)}
        >
          <DirectRuntime runtime={peer.runtime} />
          <span className="stack dm-identity">
            <span className="row-name">
              {peer.alias}
              <span className="row-suffix">
                {" "}
                · #{peer.workspaceLabel.replace(/^#/, "")}
              </span>
            </span>
            <span className="row-reason">
              {peer.paneLabel?.text
                ? `${peer.paneLabel.text}${peer.paneLabel.truncated ? "…" : ""} · `
                : ""}
              {presenceWord[peer.activity]}
            </span>
          </span>
          {peer.attentionCount > 0 ? (
            <span
              className="dot dot-alert"
              role="img"
              aria-label={`${peer.attentionCount} need you`}
            />
          ) : unread ? (
            <span
              className="dot dot-unread"
              role="img"
              aria-label="Unread direct message"
            />
          ) : null}
        </button>
      </li>
    );
  }
  return (
    <nav className="nav-section dm-navigation" aria-label="Direct messages">
      <p className="nav-label">
        Direct messages<span className="count">{dm.peers.length}</span>
      </p>
      <input
        className="dm-filter"
        type="search"
        aria-label="Filter direct messages"
        placeholder="Find an agent or workspace"
        value={filter}
        onChange={(event) => setFilter(event.target.value)}
      />
      {dm.directoryError && (
        <p className="dm-status" role="status">
          Couldn't update agents.{" "}
          <button type="button" className="text-button" onClick={dm.retry}>
            Try again
          </button>
        </p>
      )}
      <ul>{active.map(row)}</ul>
      {retired.length > 0 && (
        <>
          <button
            type="button"
            className="group-header"
            aria-expanded={closed}
            onClick={() => setClosed(!closed)}
          >
            Closed<span className="count">{retired.length}</span>
          </button>
          {closed && <ul>{retired.map(row)}</ul>}
        </>
      )}
      {peers.length === 0 && (
        <p className="dm-status">
          {filter ? "No matching agents" : "Named agents appear here."}
        </p>
      )}
    </nav>
  );
}
