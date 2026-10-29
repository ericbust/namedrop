import { type KeyboardEvent, useLayoutEffect, useRef } from "react";
import { attentionCopy, composerCopy } from "../copy.ts";
import type { Channel, SpaceKey, State } from "../model.ts";
import { channelSuffix, connectionStatus, sortedChannels } from "../store.ts";
import { Icon } from "./Icons.tsx";

function reason(channel: Channel): string {
  const [first] = channel.attention;
  if (!first) return "Something needs you";
  const more = channel.attention.length - 1;
  return `${attentionCopy(first).title}${more > 0 ? ` · ${more} more` : ""}`;
}
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
      state.selection.key !== null
    ) {
      sidebar.current
        ?.querySelector<HTMLButtonElement>(
          `.channels-navigation button[data-channel-key="${state.selection.key}"]`,
        )
        ?.focus();
    }
  });
  const needsYou = channels.filter(
    (channel) => channel.health.level === "needs_you",
  );
  const notSent = new Set<SpaceKey>();
  for (const draft of state.drafts.values())
    if (draft.failure !== null) notSent.add(draft.key);
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
  function tabKey(items: Channel[]) {
    return items.some((channel) => channel.key === state.selection.key)
      ? state.selection.key
      : items[0]?.key;
  }
  function name(channel: Channel) {
    const suffix = channelSuffix(channel, channels);
    return (
      <span className="row-name">
        {channel.displayName}
        {suffix !== null && <span className="row-suffix"> · {suffix}</span>}
      </span>
    );
  }
  const needsYouTab = tabKey(needsYou);
  const channelsTab = tabKey(channels);
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
        <span className="brand-mark" aria-hidden="true" />
        <span className="brand-name">Namedrop</span>
      </header>
      <div className="nav">
        {needsYou.length > 0 && (
          <nav
            aria-label="Needs you"
            className="nav-section attention-navigation"
            onKeyDown={move}
          >
            <p className="nav-label">
              Needs you<span className="count">{needsYou.length}</span>
            </p>
            <ul>
              {needsYou.map((channel) => (
                <li key={channel.key}>
                  <button
                    type="button"
                    className="row attention-row"
                    data-channel-key={channel.key}
                    tabIndex={needsYouTab === channel.key ? 0 : -1}
                    onClick={() => onSelect(channel.key)}
                  >
                    <span className="stack">
                      {name(channel)}
                      <span className="row-reason">{reason(channel)}</span>
                    </span>
                    <span
                      className="dot dot-alert"
                      role="img"
                      aria-label="Needs you"
                    />
                  </button>
                </li>
              ))}
            </ul>
          </nav>
        )}
        <nav
          aria-label="Channels"
          className="nav-section channels-navigation"
          onKeyDown={move}
        >
          <p className="nav-label">
            Channels<span className="count">{channels.length}</span>
          </p>
          {channels.length === 0 && connection !== "live" ? (
            <div className="skeleton" aria-hidden="true">
              <div className="skeleton-row" />
              <div className="skeleton-row" />
              <div className="skeleton-row" />
            </div>
          ) : (
            <ul>
              {channels.map((channel) => {
                const unread = state.unread.has(channel.key);
                return (
                  <li key={channel.key}>
                    <button
                      type="button"
                      className={`row${unread ? " unread" : ""}`}
                      data-channel-key={channel.key}
                      aria-current={
                        state.selection.key === channel.key ? "page" : undefined
                      }
                      tabIndex={channelsTab === channel.key ? 0 : -1}
                      onClick={() => onSelect(channel.key)}
                    >
                      <Icon name="hash" className="hash" />
                      {name(channel)}
                      {notSent.has(channel.key) && (
                        <span
                          className="row-failed"
                          role="img"
                          aria-label={composerCopy.postNotSent}
                        >
                          <Icon name="alert" className="icon-14" />
                        </span>
                      )}
                      {channel.health.level === "needs_you" ? (
                        <span
                          className="dot dot-alert"
                          role="img"
                          aria-label="Needs you"
                        />
                      ) : (
                        unread && (
                          <span
                            className="dot dot-unread"
                            role="img"
                            aria-label="Unread activity"
                          />
                        )
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </nav>
      </div>
      <footer className="sidebar-footer">
        <span className={`connection-dot ${connection}`} aria-hidden="true" />
        {connection === "live"
          ? "Live"
          : connection === "connecting"
            ? "Connecting…"
            : "Reconnecting…"}
      </footer>
    </aside>
  );
}
