// Inline icons (from the reviewed mockup).

/** Props of every icon */
type IconProps = { className?: string };

/** The app's mark: two screens and a pointer crossing over */
export function Logo({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" className={className}>
      <rect x="2" y="4.5" width="12" height="9" rx="2" fill="none" stroke="currentColor" strokeWidth="1.8" />
      <rect x="10" y="10.5" width="12" height="9" rx="2" fill="currentColor" opacity=".3" />
      <path d="M13 8.5l7.5 3.3-3.2 1-1.1 3.2z" fill="currentColor" />
    </svg>
  );
}

/** Windows */
export function WindowsIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 16 16" className={className}>
      <path
        fill="currentColor"
        d="M1.5 3.2 6.8 2.5v5H1.5zM7.6 2.4 14.5 1.5v6H7.6zM1.5 8.3h5.3v5.1l-5.3-.7zM7.6 8.3h6.9v6.2l-6.9-.9z"
      />
    </svg>
  );
}

/** A platform's glyph */
export function PlatformIcon({ platform }: { platform: string }) {
  return platform === "macos" ? <span className="g-mac">⌘</span> : <WindowsIcon />;
}

/** The system's logo, one color: the apple, or the four panes */
export function OsLogo({ platform }: { platform: string }) {
  return platform === "macos" ? (
    <svg viewBox="0 0 24 24">
      <path
        fill="currentColor"
        d="M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701"
      />
    </svg>
  ) : (
    <svg viewBox="0 0 24 24">
      <path fill="currentColor" d="M1 1h10.5v10.5H1zM12.5 1H23v10.5H12.5zM1 12.5h10.5V23H1zM12.5 12.5H23V23H12.5z" />
    </svg>
  );
}

/** Two screens side by side: arranging them */
export function ArrangeIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <rect x="1.5" y="3" width="7" height="6" rx="1.4" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <rect x="8.5" y="6" width="6" height="7" rx="1.4" fill="currentColor" opacity=".35" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

/** Three dots: more actions */
export function MoreIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <circle cx="3.5" cy="8" r="1.3" fill="currentColor" />
      <circle cx="8" cy="8" r="1.3" fill="currentColor" />
      <circle cx="12.5" cy="8" r="1.3" fill="currentColor" />
    </svg>
  );
}

/** A chevron pointing down: expand */
export function ChevronIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** A gear: general settings */
export function GearIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <circle cx="8" cy="8" r="2.3" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Arrows both ways: switching */
export function SwitchIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path
        d="M2 5h11M10 2l3 3-3 3M14 11H3M6 8l-3 3 3 3"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** An eye: appearance */
export function EyeIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8z" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="8" cy="8" r="2" fill="currentColor" />
    </svg>
  );
}

/** Pause */
export function PauseIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <rect x="3.5" y="3" width="3" height="10" rx="1" fill="currentColor" />
      <rect x="9.5" y="3" width="3" height="10" rx="1" fill="currentColor" />
    </svg>
  );
}

/** Play / resume */
export function PlayIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M5 3l8 5-8 5z" fill="currentColor" />
    </svg>
  );
}

/** Lock */
export function LockIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <rect x="3" y="7" width="10" height="7" rx="1.6" fill="currentColor" />
      <path d="M5.2 7V5.2a2.8 2.8 0 015.6 0V7" stroke="currentColor" strokeWidth="1.6" fill="none" />
    </svg>
  );
}

/** Arrow out: controlling another device */
export function OutIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M2 8h10M8.5 4.5 12 8l-3.5 3.5" stroke="currentColor" strokeWidth="1.8" fill="none" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** Pointer */
export function CursorIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M3 2l10 5.2-4.3 1.1L6.8 13z" fill="currentColor" />
    </svg>
  );
}

/** Check mark */
export function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M3 8.5l3.2 3L13 4.5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** Cross */
export function CrossIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

/** Warning */
export function WarnIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M8 1.8l6.5 11.4H1.5z" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
      <path d="M8 6.3v3.4M8 11.3v.2" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

/** Information */
export function InfoIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <circle cx="8" cy="8" r="6.3" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M8 7.2v4M8 4.8v.2" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
    </svg>
  );
}

/** A hand: Accessibility */
export function HandIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M8 2v5.5M5.5 3.5v5M10.5 3v5M13 5v4.5a4.5 4.5 0 01-9 0V7" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" />
    </svg>
  );
}

/** A keyboard: Input Monitoring */
export function KeyboardIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <rect x="1.5" y="4" width="13" height="8" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M4 7h1M7 7h1M10 7h2M5 9.5h6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

