import {
  type KeyboardEvent,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
} from "react";
import { activityCopy, attentionCopy, composerCopy } from "../copy.ts";
import type { Channel, SpaceKey, State } from "../model.ts";
import { channelSuffix, connectionStatus, sortedChannels } from "../store.ts";
import { routePath } from "../wire.ts";
import { Icon } from "./Icons.tsx";

function reason(channel: Channel): string {
  const [first] = channel.attention;
  if (!first) return "Something needs you";
  const more = channel.attention.length - 1;
  return `${attentionCopy(first).title}${more > 0 ? ` · ${more} more` : ""}`;
}
function needsYouOf(channels: Channel[]): Channel[] {
  return channels.filter((channel) => channel.health.level === "needs_you");
}
const TABBABLE = "a[href], button:not([disabled])";
const CURRENT_ROW =
  '.channels-navigation [aria-current="page"], .activity-entry[aria-current="page"]';
function focusCurrentRow(sidebar: HTMLElement | null) {
  (
    sidebar?.querySelector<HTMLElement>(CURRENT_ROW) ??
    sidebar?.querySelector<HTMLElement>(TABBABLE)
  )?.focus();
}

// On a phone the sidebar is a drawer, and the connection dot moves to the
// button that opens it.
type Drawer = { open: boolean; onClose: () => void };
type Props = {
  state: State;
  busy: number;
  id: string;
  drawer: Drawer | null;
  toggleFocusLost: RefObject<boolean>;
  onSelect: (key: SpaceKey) => void;
  onActivity: () => void;
};
export function Sidebar({
  state,
  busy,
  id,
  drawer,
  toggleFocusLost,
  onSelect,
  onActivity,
}: Props) {
  const sidebar = useRef<HTMLElement>(null);
  const focusedRow = useRef<HTMLButtonElement | null>(null);
  const hiddenWithFocus = useRef(false);
  const connection = connectionStatus(state);
  const channels = sortedChannels(state);
  const modal = drawer?.open === true;
  const close = drawer?.onClose;
  const phone = drawer !== null;
  // The column hides as the window narrows to a phone, and the Channels button
  // goes away as it widens. The focus either one held falls to the page, so the
  // one that now shows takes it, whether the blur comes before the new layout
  // renders or after it.
  useLayoutEffect(() => {
    const lost = phone ? hiddenWithFocus : toggleFocusLost;
    if (!lost.current) return;
    lost.current = false;
    if (document.activeElement !== document.body) return;
    if (phone) document.querySelector<HTMLElement>(".channels-toggle")?.focus();
    else focusCurrentRow(sidebar.current);
  }, [phone, toggleFocusLost]);
  useLayoutEffect(() => {
    if (modal) focusCurrentRow(sidebar.current);
  }, [modal]);
  useEffect(() => {
    if (!modal || close === undefined) return;
    const onEscape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      close();
    };
    document.addEventListener("keydown", onEscape);
    return () => document.removeEventListener("keydown", onEscape);
  }, [modal, close]);
  function trap(event: KeyboardEvent<HTMLElement>) {
    if (event.key !== "Tab") return;
    const stops = [
      ...event.currentTarget.querySelectorAll<HTMLElement>(TABBABLE),
    ].filter((element) => element.tabIndex >= 0);
    const at = stops.indexOf(document.activeElement as HTMLElement);
    const edge = event.shiftKey ? 0 : stops.length - 1;
    if (at !== -1 && at !== edge) return;
    event.preventDefault();
    (event.shiftKey ? stops.at(-1) : stops[0])?.focus();
  }
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
  const needsYou = needsYouOf(channels);
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
    // In the drawer, choosing a channel closes it, so arrows only move.
    if (drawer === null) target?.click();
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
    // biome-ignore lint/a11y/useAriaPropsSupportedByRole: aria-modal is set only while the role is dialog.
    <aside
      className="sidebar"
      id={id}
      ref={sidebar}
      role={modal ? "dialog" : undefined}
      aria-label={modal ? "Channels" : undefined}
      aria-modal={modal ? true : undefined}
      tabIndex={modal ? -1 : undefined}
      onKeyDown={modal ? trap : undefined}
      onFocusCapture={(event) => {
        const target = event.target;
        focusedRow.current =
          target instanceof HTMLButtonElement &&
          target.hasAttribute("data-channel-key")
            ? target
            : null;
      }}
      onBlurCapture={(event) => {
        focusedRow.current = null;
        const hidden =
          event.relatedTarget === null &&
          !event.currentTarget.checkVisibility({ visibilityProperty: true });
        hiddenWithFocus.current = hidden && !phone;
        if (hidden && phone)
          requestAnimationFrame(() => {
            if (document.activeElement === document.body)
              document.querySelector<HTMLElement>(".channels-toggle")?.focus();
          });
      }}
    >
      <header className="brand">
        <span className="brand-mark" aria-hidden="true" />
        <span className="brand-name">Namedrop</span>
      </header>
      <div className="nav">
        <a
          className="row activity-entry"
          href={routePath({ kind: "activity" })}
          aria-current={
            state.selection.kind === "activity" ? "page" : undefined
          }
          onClick={(event) => {
            if (
              event.button !== 0 ||
              event.metaKey ||
              event.ctrlKey ||
              event.shiftKey ||
              event.altKey
            )
              return;
            event.preventDefault();
            onActivity();
          }}
        >
          <Icon name="pulse" className="row-icon" />
          <span className="row-name">{activityCopy.title}</span>
          {busy > 0 && <span className="count">{busy}</span>}
        </a>
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
          aria-label="Channel list"
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
        {drawer === null && (
          <span className={`connection-dot ${connection}`} aria-hidden="true" />
        )}
        {connection === "live"
          ? "Live"
          : connection === "connecting"
            ? "Connecting…"
            : "Reconnecting…"}
      </footer>
    </aside>
  );
}

// `focusLost` is set while focus that was on the button has gone nowhere:
// the button hid with it, or unmounted with it as the phone layout ended.
export function ChannelsToggle({
  state,
  open,
  controls,
  focusLost,
  onOpen,
}: {
  state: State;
  open: boolean;
  controls: string;
  focusLost: RefObject<boolean>;
  onOpen: () => void;
}) {
  const needsYou = needsYouOf(sortedChannels(state)).length;
  const button = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    const element = button.current;
    // On a phone the button moves to another header as a pane opens or closes
    // and as back or forward switches views. The one that mounts takes the
    // focus the one that left held, then starts clean. A claim can't take
    // focus from a drawer row, because the header is inert under the drawer.
    if (focusLost.current) element?.focus();
    focusLost.current = false;
    return () => {
      if (document.activeElement === element) focusLost.current = true;
    };
  }, [focusLost]);
  return (
    <button
      ref={button}
      type="button"
      className="channels-toggle"
      aria-label={
        needsYou === 0
          ? "Channels"
          : `Channels, ${needsYou} need${needsYou === 1 ? "s" : ""} you`
      }
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-controls={controls}
      onClick={onOpen}
      onBlur={(event) => {
        if (
          event.relatedTarget === null &&
          !event.currentTarget.checkVisibility({ visibilityProperty: true })
        )
          focusLost.current = true;
      }}
    >
      <Icon name="menu" />
      {needsYou > 0 && <span className="dot dot-alert" />}
      <span className={`connection-dot ${connectionStatus(state)}`} />
    </button>
  );
}
