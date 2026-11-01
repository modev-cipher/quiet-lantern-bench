type IconProps = {
  className?: string;
};

function base(className?: string) {
  return {
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.5,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    className: className ?? "h-5 w-5",
    "aria-hidden": true,
  };
}

export function LibraryIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H9v16H5.5A1.5 1.5 0 0 1 4 18.5Z" />
      <path d="M9 4h4.5A1.5 1.5 0 0 1 15 5.5v13A1.5 1.5 0 0 1 13.5 20H9Z" />
      <path d="m15.4 5 3.2.9a1.5 1.5 0 0 1 1 1.85l-3.2 11.2" />
    </svg>
  );
}

export function JobsIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M12 21a9 9 0 1 0-9-9" />
      <path d="M12 7v5l3 2" />
      <path d="M3 12H1.5M4.2 6.2 3 5" />
    </svg>
  );
}

export function ModelsIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M12 3 20 7.5v9L12 21 4 16.5v-9Z" />
      <path d="M12 12 20 7.5M12 12v9M12 12 4 7.5" />
    </svg>
  );
}

export function SettingsIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2.5v2.2M12 19.3v2.2M4.2 7l1.9 1.1M17.9 15.9l1.9 1.1M4.2 17l1.9-1.1M17.9 8.1 19.8 7" />
    </svg>
  );
}

// Direction-bearing: mirrors under [dir="rtl"] via the single .mirror-rtl utility (I18N-1).
export function ChevronIcon({ className }: IconProps) {
  return (
    <svg
      {...base(className)}
      className={`mirror-rtl ${className ?? "h-5 w-5"}`}
    >
      <path d="m9 6 6 6-6 6" />
    </svg>
  );
}

export function CloseIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="m6 6 12 12M18 6 6 18" />
    </svg>
  );
}

export function PlayIcon({ className }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className ?? "h-4 w-4"}
      aria-hidden
    >
      <path d="M7 4.8v14.4a1 1 0 0 0 1.54.84l11-7.2a1 1 0 0 0 0-1.68l-11-7.2A1 1 0 0 0 7 4.8Z" />
    </svg>
  );
}

export function PauseIcon({ className }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className ?? "h-4 w-4"}
      aria-hidden
    >
      <rect x="6" y="4.5" width="4" height="15" rx="1.5" />
      <rect x="14" y="4.5" width="4" height="15" rx="1.5" />
    </svg>
  );
}

export function CheckIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")} strokeWidth={2}>
      <path d="m4 12.5 5 5L20 6.5" />
    </svg>
  );
}

export function CircleIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <circle cx="12" cy="12" r="8" />
    </svg>
  );
}

export function UploadIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M12 15V4M12 4 8 8M12 4l4 4" />
      <path d="M4 15v3.5A1.5 1.5 0 0 0 5.5 20h13a1.5 1.5 0 0 0 1.5-1.5V15" />
    </svg>
  );
}

export function DownloadIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M12 4v11M12 15l-4-4M12 15l4-4" />
      <path d="M4 19h16" />
    </svg>
  );
}

export function StarIcon({ className }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className ?? "h-3.5 w-3.5"}
      aria-hidden
    >
      <path d="M12 2.6l2.6 5.9 6.4.6-4.8 4.2 1.4 6.3L12 16.9 6.4 19.6l1.4-6.3L3 9.1l6.4-.6Z" />
    </svg>
  );
}

export function TrashIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-3.5 w-3.5")}>
      <path d="M4 6.5h16M9 6.5V5a1.5 1.5 0 0 1 1.5-1.5h3A1.5 1.5 0 0 1 15 5v1.5" />
      <path d="M6.5 6.5l.8 12a1.5 1.5 0 0 0 1.5 1.4h6.4a1.5 1.5 0 0 0 1.5-1.4l.8-12" />
      <path d="M10 10.5v6M14 10.5v6" />
    </svg>
  );
}

export function SkipBackIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M11 6.5 4.5 11a1.2 1.2 0 0 0 0 2L11 17.5" />
      <path d="M19 6.5 12.5 11a1.2 1.2 0 0 0 0 2L19 17.5" />
    </svg>
  );
}

export function SkipForwardIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M13 6.5 19.5 11a1.2 1.2 0 0 1 0 2L13 17.5" />
      <path d="M5 6.5 11.5 11a1.2 1.2 0 0 1 0 2L5 17.5" />
    </svg>
  );
}

export function VolumeIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M4 9.5v5h3l4 3.5V6L7 9.5Z" />
      <path d="M15 9a4 4 0 0 1 0 6M17.5 6.5a7 7 0 0 1 0 11" />
    </svg>
  );
}

export function MuteIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M4 9.5v5h3l4 3.5V6L7 9.5Z" />
      <path d="m15 9.5 5 5M20 9.5l-5 5" />
    </svg>
  );
}

export function AudioIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M9 18V6l10-2v12" />
      <circle cx="6" cy="18" r="3" />
      <circle cx="16" cy="16" r="3" />
    </svg>
  );
}

export function FolderIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2.2h7A1.5 1.5 0 0 1 19 9.7v7.8A1.5 1.5 0 0 1 17.5 19h-13A1.5 1.5 0 0 1 3 17.5Z" />
    </svg>
  );
}

export function AlertIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M12 8.5v4.2" />
      <path d="M12 16h.01" />
      <path d="M10.3 3.9 2.5 17.5a2 2 0 0 0 1.7 3h15.6a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z" />
    </svg>
  );
}

export function CastIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M4 8V6.5A1.5 1.5 0 0 1 5.5 5h13A1.5 1.5 0 0 1 20 6.5v11a1.5 1.5 0 0 1-1.5 1.5H14" />
      <path d="M4 13a6 6 0 0 1 6 6M4 16.5A2.5 2.5 0 0 1 6.5 19M4 20h.01" />
    </svg>
  );
}

export function MoreIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M12 5.5h.01M12 12h.01M12 18.5h.01" strokeWidth={2.5} />
    </svg>
  );
}

export function SparkIcon({ className }: IconProps) {
  return (
    <svg {...base(className ?? "h-4 w-4")}>
      <path d="M12 3.5 13.6 8l4.4 1.6-4.4 1.6L12 15.7l-1.6-4.5L6 9.6 10.4 8Z" />
      <path d="M18 15.5l.7 1.9 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7Z" />
    </svg>
  );
}

export function SunIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2.5v2.5M12 19v2.5M2.5 12H5M19 12h2.5M4.9 4.9l1.8 1.8M17.3 17.3l1.8 1.8M19.1 4.9l-1.8 1.8M6.7 17.3l-1.8 1.8" />
    </svg>
  );
}

const TONE = { fill: "currentColor", fillOpacity: 0.16, stroke: "none" };

export function MicIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect {...TONE} x="8.75" y="2.75" width="6.5" height="11.5" rx="3.25" />
      <rect x="8.75" y="2.75" width="6.5" height="11.5" rx="3.25" />
      <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M9.25 21h5.5" />
    </svg>
  );
}

export function MicOffIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect {...TONE} x="8.75" y="2.75" width="6.5" height="11.5" rx="3.25" />
      <rect x="8.75" y="2.75" width="6.5" height="11.5" rx="3.25" />
      <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M9.25 21h5.5" />
      <path className="ui-slash-gap" d="m4.5 4.5 15 15" />
      <path d="m4.5 4.5 15 15" />
    </svg>
  );
}

export function KeyboardIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect {...TONE} x="2.75" y="5.75" width="18.5" height="12.5" rx="3.5" />
      <rect x="2.75" y="5.75" width="18.5" height="12.5" rx="3.5" />
      <path d="M8.5 14.5h7" />
      <circle cx="7" cy="10" r="1" fill="currentColor" stroke="none" />
      <circle cx="10.33" cy="10" r="1" fill="currentColor" stroke="none" />
      <circle cx="13.67" cy="10" r="1" fill="currentColor" stroke="none" />
      <circle cx="17" cy="10" r="1" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function PowerIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M12 3.5v7" />
      <path d="M7.2 6.4a7.25 7.25 0 1 0 9.6 0" />
    </svg>
  );
}

export function StopIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect x="7" y="7" width="10" height="10" rx="3" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function SendIcon({ className }: IconProps) {
  return (
    <svg
      {...base(className)}
      className={`mirror-rtl ${className ?? "h-5 w-5"}`}
    >
      <path d="M5 12h13.5M13 6.5l5.5 5.5-5.5 5.5" />
    </svg>
  );
}

export function PageIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path {...TONE} d="M6.75 2.75h7.5l4 4v14.5H6.75Z" />
      <path d="M6.75 2.75h7.5l4 4v14.5H6.75Z" />
      <path d="M14.25 2.75v4h4M9.5 12h5.5M9.5 15.5h5.5" />
    </svg>
  );
}

export function TargetIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="12" r="4.25" />
      <circle cx="12" cy="12" r="0.9" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function InfoIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="8.75" />
      <path d="M12 11v5.25" />
      <circle cx="12" cy="7.75" r="0.9" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function CopyIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
      <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
    </svg>
  );
}

export function HelpCircleIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="10" />
      <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  );
}

export function BugIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect width="8" height="14" x="8" y="6" rx="4" />
      <path d="m19 7-3 2" />
      <path d="m5 7 3 2" />
      <path d="m19 19-3-2" />
      <path d="m5 19 3-2" />
      <path d="M20 13h-4" />
      <path d="M4 13h4" />
      <path d="m10 4 1 2" />
      <path d="m14 4-1 2" />
    </svg>
  );
}

export function GlobeIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="12" cy="12" r="10" />
      <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
      <path d="M2 12h20" />
    </svg>
  );
}

export function FileTextIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
      <path d="M14 2v4a2 2 0 0 0 2 2h4" />
      <path d="M10 9H8" />
      <path d="M16 13H8" />
      <path d="M16 17H8" />
    </svg>
  );
}

export function ShieldIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
    </svg>
  );
}

export function PopOutIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect x="3.5" y="7.5" width="13" height="13" rx="2" />
      <path d="M13 3.5h7.5V11M20.5 3.5 11 13" />
    </svg>
  );
}

export function DockSideIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="M14.5 4.5v15" />
    </svg>
  );
}

export function MinimizeIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M5.5 12h13" />
    </svg>
  );
}

export function MoonIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z" />
    </svg>
  );
}

export function BooksIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M5 4.5v15M9.5 4.5v15M14 5.5l4.2 13.8M3.5 19.5h17" />
    </svg>
  );
}

export function HeadphonesIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M4 15.5V12a8 8 0 0 1 16 0v3.5" />
      <rect x="3.5" y="14" width="4" height="6" rx="1.5" />
      <rect x="16.5" y="14" width="4" height="6" rx="1.5" />
    </svg>
  );
}

export function SearchIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m20 20-4.2-4.2" />
    </svg>
  );
}

export function ChatIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M4.5 6.5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2H10l-4.5 3.5v-3.5h0a1 1 0 0 1-1-1Z" />
    </svg>
  );
}

export function WaveIcon({ className }: IconProps) {
  return (
    <svg {...base(className)}>
      <path d="M4 12h2.5l2-5 3 10 3-13 2.5 8H20" />
    </svg>
  );
}
