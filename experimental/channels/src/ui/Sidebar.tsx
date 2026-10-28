import { type KeyboardEvent, useLayoutEffect, useRef } from "react";
import type { Channel, SpaceKey, State } from "../model.ts";
import { channelName, connectionStatus, sortedChannels } from "../store.ts";

type Props = { state: State; onSelect: (key: SpaceKey) => void };
export function Sidebar({ state, onSelect }: Props) {
  const sidebar = useRef<HTMLElement>(null);
  const focusedRow = useRef<HTMLButtonElement | null>(null);
  const connection = connectionStatus(state);
  const channels = sortedChannels(state);
  useLayoutEffect(() => {
    const row = focusedRow.current;
    if (
      row &&
      !row.isConnected &&
      document.activeElement === document.body &&
      state.selectedKey !== null
    ) {
      sidebar.current
        ?.querySelector<HTMLButtonElement>(
          `.channels-navigation button[data-channel-key="${state.selectedKey}"]`,
        )
        ?.focus();
    }
  });
  const needsYou = channels.filter(
    (channel) => channel.health.level === "needs_you",
  );
  function move(event: KeyboardEvent<HTMLElement>) {
    const buttons = [
      ...event.currentTarget.querySelectorAll<HTMLButtonElement>(
        "button[data-channel-key]",
      ),
    ];
    const active = document.activeElement;
    const index =
      active instanceof HTMLButtonElement ? buttons.indexOf(active) : -1;
    let target: HTMLButtonElement | undefined;
    switch (event.key) {
      case "ArrowDown":
        target = buttons[(index + 1) % buttons.length];
        break;
      case "ArrowUp":
        target = buttons[(index - 1 + buttons.length) % buttons.length];
        break;
      case "Home":
        target = buttons[0];
        break;
      case "End":
        target = buttons.at(-1);
        break;
      default:
        return;
    }
    event.preventDefault();
    target?.focus();
    target?.click();
  }
  function rows(items: Channel[]) {
    const tabKey = items.some((channel) => channel.key === state.selectedKey)
      ? state.selectedKey
      : items[0]?.key;
    return items.map((channel) => (
      <li key={channel.key}>
        <button
          type="button"
          className={`channel-button${state.unread.has(channel.key) ? " is-unread" : ""}`}
          data-channel-key={channel.key}
          aria-current={state.selectedKey === channel.key ? "page" : undefined}
          tabIndex={tabKey === channel.key ? 0 : -1}
          onClick={() => onSelect(channel.key)}
        >
          <span className="channel-identity">
            <span className="channel-name">
              {channelName(channel, channels)}
            </span>
            <span className="channel-token">
              {channel.addressableLabel === null
                ? "Not addressable"
                : `#${channel.addressableLabel}`}
            </span>
          </span>
          {channel.health.level === "needs_you" && (
            <span
              className="presence-dot"
              data-activity="blocked"
              role="img"
              aria-label="Needs you"
            />
          )}
          {state.unread.has(channel.key) && (
            <span
              className="unread-dot"
              role="img"
              aria-label="Unread activity"
            />
          )}
        </button>
      </li>
    ));
  }
  return (
    <aside
      className="sidebar"
      ref={sidebar}
      onFocusCapture={(event) => {
        const target = event.target;
        focusedRow.current =
          target instanceof HTMLButtonElement &&
          target.hasAttribute("data-channel-key")
            ? target
            : null;
      }}
      onBlurCapture={() => {
        focusedRow.current = null;
      }}
    >
      <header className="brand">
        <span className="brand-name">Namedrop</span>
        <span className="brand-description">Your workspaces, together.</span>
      </header>
      <nav
        aria-label="Needs you"
        className="sidebar-section attention-navigation"
        onKeyDown={move}
      >
        <h2>
          Needs you<span className="section-count">{needsYou.length}</span>
        </h2>
        {needsYou.length > 0 ? (
          <ul>{rows(needsYou)}</ul>
        ) : (
          <p className="sidebar-note">Nothing needs your attention.</p>
        )}
      </nav>
      <nav
        aria-label="Channels"
        className="sidebar-section channels-navigation"
        onKeyDown={move}
      >
        <h2>
          Channels<span className="section-count">{channels.length}</span>
        </h2>
        <ul>{rows(channels)}</ul>
        {channels.length === 0 && (
          <p className="sidebar-note">
            {connection === "live"
              ? "No live workspaces."
              : "Waiting for workspaces…"}
          </p>
        )}
      </nav>
      <footer className="sidebar-footer">
        <span className={`connection-dot ${connection}`} aria-hidden="true" />
        {connection === "live"
          ? "Live updates"
          : connection === "connecting"
            ? "Connecting…"
            : "Reconnecting…"}
        <span className="read-only">Read only</span>
      </footer>
    </aside>
  );
}
