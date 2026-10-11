import type { SettingsSection } from "../state/store";

const iconProps = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: "1.9",
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
} as const;

/* The More dialog's page icons (views/MoreDialog.tsx), shared so anything
   else leading to one of its pages — the conversation search's Settings
   shortcuts — shows the same icon for it. */

export const ModelsIcon = () => (
  <svg {...iconProps}>
    <path d="M12 3.5 19.5 7.5v9L12 20.5 4.5 16.5v-9Z" />
    <path d="M4.5 7.5 12 11.5l7.5-4M12 11.5v9" />
  </svg>
);

export const CompassIcon = () => (
  <svg {...iconProps}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M14.8 9.2 13 13l-3.8 1.8L11 11l3.8-1.8Z" />
  </svg>
);

export const PuzzleIcon = () => (
  <svg {...iconProps}>
    <path d="M9.5 3.5a1.6 1.6 0 0 1 3.1 0c0 .5-.2.9-.4 1.3a.6.6 0 0 0 .5.9H15a1.5 1.5 0 0 1 1.5 1.5v2.4a.6.6 0 0 0 .9.5c.4-.3.8-.4 1.3-.4a1.6 1.6 0 0 1 0 3.1c-.5 0-.9-.2-1.3-.4a.6.6 0 0 0-.9.5V16A1.5 1.5 0 0 1 15 17.5h-2.4a.6.6 0 0 1-.5-.9c.2-.4.4-.8.4-1.3a1.6 1.6 0 0 0-3.1 0c0 .5.2.9.4 1.3a.6.6 0 0 1-.5.9H6.5A1.5 1.5 0 0 1 5 16v-2.3a.6.6 0 0 0-.9-.5c-.4.2-.8.4-1.3.4a1.6 1.6 0 0 1 0-3.1c.5 0 .9.1 1.3.4a.6.6 0 0 0 .9-.5V7.7A1.5 1.5 0 0 1 6.5 6.2H9a.6.6 0 0 0 .5-.9c-.2-.4-.4-.8-.4-1.3Z" />
  </svg>
);

export const SkillIcon = () => (
  <svg {...iconProps}>
    <path d="M12 4.5v3M12 16.5v3M4.5 12h3M16.5 12h3M7 7l2 2M15 15l2 2M17 7l-2 2M9 15l-2 2" />
    <circle cx="12" cy="12" r="2.4" />
  </svg>
);

const SlidersIcon = () => (
  <svg {...iconProps}>
    <path d="M4.5 7.5h8M16.5 7.5h3M4.5 16.5h3M11.5 16.5h8" />
    <circle cx="14.5" cy="7.5" r="2" />
    <circle cx="9.5" cy="16.5" r="2" />
  </svg>
);

const ChatIcon = () => (
  <svg {...iconProps}>
    <path d="M4.5 5.5h15a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H10l-4 3v-3H4.5a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1Z" />
  </svg>
);

const ChipIcon = () => (
  <svg {...iconProps}>
    <rect x="6.5" y="6.5" width="11" height="11" rx="1.5" />
    <rect x="10" y="10" width="4" height="4" rx=".5" />
    <path d="M10 3.5v3M14 3.5v3M10 17.5v3M14 17.5v3M3.5 10h3M3.5 14h3M17.5 10h3M17.5 14h3" />
  </svg>
);

const ShieldIcon = () => (
  <svg {...iconProps}>
    <path d="M12 3.5 5 6.2v5.3c0 4.1 2.9 7.6 7 8.9 4.1-1.3 7-4.8 7-8.9V6.2Z" />
    <path d="m9.2 12 2 2 3.6-3.8" />
  </svg>
);

const DevicesIcon = () => (
  <svg {...iconProps}>
    <rect x="2.5" y="5" width="12.5" height="9" rx="1.3" />
    <path d="M6.5 18.5h4.5M8.75 14v4.5" />
    <rect x="16.5" y="8.5" width="5" height="10.5" rx="1.2" />
  </svg>
);

const DatabaseIcon = () => (
  <svg {...iconProps}>
    <ellipse cx="12" cy="6.5" rx="7" ry="2.7" />
    <path d="M5 6.5v11c0 1.5 3.1 2.7 7 2.7s7-1.2 7-2.7v-11" />
    <path d="M5 12c0 1.5 3.1 2.7 7 2.7s7-1.2 7-2.7" />
  </svg>
);

const CodeIcon = () => (
  <svg {...iconProps}>
    <path d="M8.5 7.5 4 12l4.5 4.5M15.5 7.5 20 12l-4.5 4.5M13.5 5l-3 14" />
  </svg>
);

/** Each Settings category's icon. */
export const SETTINGS_ICONS = {
  general: SlidersIcon,
  chat: ChatIcon,
  model: ChipIcon,
  privacy: ShieldIcon,
  devices: DevicesIcon,
  data: DatabaseIcon,
  advanced: CodeIcon,
} satisfies Record<SettingsSection, unknown>;
