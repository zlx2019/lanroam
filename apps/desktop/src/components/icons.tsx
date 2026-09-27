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

/** Arrow in: controlled by another device */
export function InIcon() {
  return (
    <svg viewBox="0 0 16 16">
      <path d="M14 8H4M7.5 4.5 4 8l3.5 3.5" stroke="currentColor" strokeWidth="1.8" fill="none" strokeLinecap="round" strokeLinejoin="round" />
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
