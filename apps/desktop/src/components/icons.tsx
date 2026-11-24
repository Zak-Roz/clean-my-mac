// Small stroke icons in the spirit of SF Symbols. 16px box, currentColor.

import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

function Icon({ size = 16, children, ...rest }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {children}
    </svg>
  );
}

export const GaugeIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M2.5 11a5.5 5.5 0 1 1 11 0" />
    <path d="M8 11 10.5 6.5" />
    <circle cx="8" cy="11" r="0.9" fill="currentColor" />
  </Icon>
);

export const SlidersIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M2.5 4.5h3.4M9.1 4.5h4.4M2.5 11.5h6.4M12.1 11.5h1.4" />
    <circle cx="7.5" cy="4.5" r="1.6" />
    <circle cx="10.5" cy="11.5" r="1.6" />
  </Icon>
);

export const ListIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M5.5 4h8M5.5 8h8M5.5 12h8" />
    <circle cx="2.75" cy="4" r="0.8" fill="currentColor" stroke="none" />
    <circle cx="2.75" cy="8" r="0.8" fill="currentColor" stroke="none" />
    <circle cx="2.75" cy="12" r="0.8" fill="currentColor" stroke="none" />
  </Icon>
);

export const ClockIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="5.75" />
    <path d="M8 4.75V8l2.25 1.5" />
  </Icon>
);

export const SearchIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="7" cy="7" r="4.25" />
    <path d="m10.25 10.25 3.25 3.25" />
  </Icon>
);

export const RevealIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="7" cy="7" r="4.25" />
    <path d="m10.25 10.25 3.25 3.25M5.25 7h3.5M7 5.25v3.5" />
  </Icon>
);

export const WarningIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M7.13 2.5a1 1 0 0 1 1.74 0l5.5 9.75a1 1 0 0 1-.87 1.5H2.5a1 1 0 0 1-.87-1.5Z" />
    <path d="M8 6.25v3" />
    <circle cx="8" cy="11.4" r="0.55" fill="currentColor" stroke="none" />
  </Icon>
);

export const InfoIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="5.75" />
    <path d="M8 7.25v3.75" />
    <circle cx="8" cy="5.1" r="0.6" fill="currentColor" stroke="none" />
  </Icon>
);

export const CheckCircleIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="5.75" />
    <path d="m5.5 8.25 1.75 1.75 3.25-3.75" />
  </Icon>
);

export const XCircleIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="5.75" />
    <path d="m6 6 4 4M10 6l-4 4" />
  </Icon>
);

export const MinusCircleIcon = (p: IconProps) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="5.75" />
    <path d="M5.5 8h5" />
  </Icon>
);

export const TrashIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M2.75 4.25h10.5M6.25 4.25V2.75h3.5v1.5M4 4.25l.6 8.6a1 1 0 0 0 1 .9h4.8a1 1 0 0 0 1-.9l.6-8.6" />
    <path d="M6.75 6.75v4.5M9.25 6.75v4.5" />
  </Icon>
);

export const ChevronIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="m6 4 4 4-4 4" />
  </Icon>
);

export const LockIcon = (p: IconProps) => (
  <Icon {...p}>
    <rect x="3.25" y="7" width="9.5" height="6.75" rx="1.5" />
    <path d="M5.25 7V5a2.75 2.75 0 0 1 5.5 0v2" />
  </Icon>
);

export const MemoryIcon = (p: IconProps) => (
  <Icon {...p}>
    <rect x="2.25" y="4.5" width="11.5" height="7" rx="1.25" />
    <path d="M5 4.5V3M8 4.5V3M11 4.5V3M5 13v-1.5M8 13v-1.5M11 13v-1.5" />
  </Icon>
);

export const SparkIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M8 2.25v2.5M8 11.25v2.5M2.25 8h2.5M11.25 8h2.5M3.9 3.9l1.75 1.75M10.35 10.35l1.75 1.75M3.9 12.1l1.75-1.75M10.35 5.65l1.75-1.75" />
  </Icon>
);

export const FolderStackIcon = (p: IconProps) => (
  <Icon {...p}>
    <path d="M2.25 5.25v6.5a1 1 0 0 0 1 1h9.5a1 1 0 0 0 1-1V6.5a1 1 0 0 0-1-1H8L6.75 4H3.25a1 1 0 0 0-1 1.25Z" />
  </Icon>
);

export const TerminalIcon = (p: IconProps) => (
  <Icon {...p}>
    <rect x="2" y="3" width="12" height="10" rx="1.5" />
    <path d="m4.75 6.25 2 1.75-2 1.75M8.5 10h2.75" />
  </Icon>
);
