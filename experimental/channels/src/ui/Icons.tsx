import type { ReactNode } from "react";
import type { IconName } from "../copy.ts";

const shapes = {
  alert: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 5v3.5M8 11h.01" />
    </>
  ),
  arrow: <path d="M3 8h10M9 4l4 4-4 4" />,
  arrowDown: <path d="M8 3v10M4 9l4 4 4-4" />,
  arrowUp: <path d="M8 13V3M4 7l4-4 4 4" />,
  bell: <path d="M4.5 11.5V7.5a3.5 3.5 0 0 1 7 0v4M3 11.5h10M6.75 13.75h2.5" />,
  cancelled: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="m3.8 12.2 8.4-8.4" />
    </>
  ),
  check: <path d="m3.5 8.5 3 3 6-7" />,
  chevron: <path d="m6 4 4 4-4 4" />,
  clock: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 5v3l2 1.5" />
    </>
  ),
  done: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="m5.5 8.2 1.8 1.8 3.4-3.8" />
    </>
  ),
  hash: <path d="M6.2 2.5 4.6 13.5M11.4 2.5 9.8 13.5M2.8 6h11M2.2 10h11" />,
  image: (
    <>
      <rect x="2.5" y="3" width="11" height="10" rx="1.5" />
      <path d="m3 11 3-3 2.5 2.5L10.5 8.5 13 11M6 6.25h.01" />
    </>
  ),
  inbox: <path d="M2.5 9.5h3l1 1.5h3l1-1.5h3M3.6 3.5h8.8l1.1 6v3.5h-11V9.5z" />,
  menu: <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" />,
  pause: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M6.5 6v4M9.5 6v4" />
    </>
  ),
  play: <path d="M5.5 3.75v8.5L12 8z" />,
  plus: <path d="M8 3v10M3 8h10" />,
  plug: <path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3" />,
  prompt: <path d="M3 4.5 6 7.5 3 10.5M8 11h5" />,
  pulse: <path d="M1.5 8.5h2.8l1.7-4 3 7.5 1.8-3.5h3.7" />,
  refresh: <path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.8V6h-3.2" />,
  reply: <path d="M6.5 4 3 7.5 6.5 11M3 7.5h6.5A3.5 3.5 0 0 1 13 11v1" />,
  speaker: (
    <>
      <path d="M2.5 6.25v3.5h2.25L8 12.5v-9L4.75 6.25z" />
      <path d="M10.5 6a2.75 2.75 0 0 1 0 4M12.25 4.25a5.25 5.25 0 0 1 0 7.5" />
    </>
  ),
  speakerOff: (
    <>
      <path d="M2.5 6.25v3.5h2.25L8 12.5v-9L4.75 6.25z" />
      <path d="m10.5 6.5 3 3M13.5 6.5l-3 3" />
    </>
  ),
  report: (
    <path d="m2.5 4.6 1.2 1.2 2-2.2M2.5 10.6l1.2 1.2 2-2.2M8 4.5h5.5M8 10.5h5.5" />
  ),
  unknown: <circle cx="8" cy="8" r="6" strokeDasharray="2.4 2.3" />,
  working: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" stroke="none" />
    </>
  ),
  x: <path d="m4 4 8 8M12 4l-8 8" />,
} satisfies Record<IconName, ReactNode>;

export function Icon({
  name,
  className,
}: {
  name: IconName;
  className?: string;
}) {
  return (
    <svg
      className={className === undefined ? "icon" : `icon ${className}`}
      viewBox="0 0 16 16"
      aria-hidden="true"
    >
      {shapes[name]}
    </svg>
  );
}
