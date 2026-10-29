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
  inbox: <path d="M2.5 9.5h3l1 1.5h3l1-1.5h3M3.6 3.5h8.8l1.1 6v3.5h-11V9.5z" />,
  pause: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M6.5 6v4M9.5 6v4" />
    </>
  ),
  plug: <path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3" />,
  prompt: <path d="M3 4.5 6 7.5 3 10.5M8 11h5" />,
  refresh: <path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.8V6h-3.2" />,
  reply: <path d="M6.5 4 3 7.5 6.5 11M3 7.5h6.5A3.5 3.5 0 0 1 13 11v1" />,
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
