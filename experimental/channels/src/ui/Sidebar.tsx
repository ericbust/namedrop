import type { KeyboardEvent } from "react";
import type { Channel, SpaceKey, State } from "../model.ts";
import { channelName, sortedChannels } from "../store.ts";

type Props = { state: State; onSelect: (key: SpaceKey) => void };
export function Sidebar({ state, onSelect }: Props) {
  const channels = sortedChannels(state);
  const needsYou = channels.filter((channel) => channel.attention.length > 0);
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
          <span className="channel-hash" aria-hidden="true">
            #
          </span>
          <span className="channel-name">{channelName(channel, channels)}</span>
          {channel.attention.length > 0 && (
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
    <aside className="sidebar">
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
            {state.connection === "live"
              ? "No live workspaces."
              : "Waiting for workspaces…"}
          </p>
        )}
      </nav>
      <footer className="sidebar-footer">
        <span
          className={`connection-dot ${state.connection}`}
          aria-hidden="true"
        />
        {state.connection === "live"
          ? "Live updates"
          : state.connection === "connecting"
            ? "Connecting…"
            : "Reconnecting…"}
        <span className="read-only">Read only</span>
      </footer>
    </aside>
  );
}
