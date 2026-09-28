// Mirrors of the DTOs in src-tauri/src/dto.rs (a change lands on both sides).

/** Everything the main window shows */
export interface Snapshot {
  device: SelfDto;
  group: GroupDto | null;
  control: ControlDto;
  input: InputDto;
}

/** This device */
export interface SelfDto {
  fingerprint: string;
  name: string;
  platform: string;
  version: string;
}

/** The desk group */
export interface GroupDto {
  id: string;
  /** Placed members in reading order, then the rest by name */
  devices: DeviceDto[];
  /** Edges between members with settings of their own */
  edges: EdgeDto[];
}

/** Modifiers of a key combination, left or right (lanroam-input config) */
export interface Mods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  /** Command on macOS, the Windows key on Windows */
  meta: boolean;
}

/** A key (HID usage) pressed with exactly these modifiers */
export interface Chord extends Mods {
  key: number;
}

/** The hotkeys; digits and arrows are fixed, only their modifiers change */
export interface Hotkeys {
  pause: Chord;
  lock: Chord;
  jump: Mods;
  step: Mods;
}

/** How the pointer crosses an edge */
export type SwitchMode = "direct" | "modifier" | "dwell";

/** The modifier to hold in modifier mode */
export type HoldKey = "shift" | "ctrl" | "alt";

/** How the pointer crosses edges unless an edge says otherwise */
export interface Switching {
  mode: SwitchMode;
  hold: HoldKey;
  dwellMs: number;
  cornerPx: number;
}

/** Where media and volume keys go while another device is controlled */
export type MediaKeys = "remote" | "local";

/** How scrolling from a controlling device is replayed here */
export interface Scrolling {
  /** Percent */
  speed: number;
  reverse: boolean;
}

/** This device's input settings (lanroam-core settings.rs) */
export interface InputSettings {
  hotkeys: Hotkeys;
  /** Combinations that stay on this machine while controlling another */
  keepLocal: Chord[];
  switching: Switching;
  mediaKeys: MediaKeys;
  scrolling: Scrolling;
}

/** Settings of the edge between two devices; null follows the defaults */
export interface EdgeSettings {
  crossable: boolean;
  cornerPx: number | null;
  mode: SwitchMode | null;
}

/** An edge with settings of its own, between members `a` and `b` */
export interface EdgeDto extends EdgeSettings {
  a: string;
  b: string;
}

/** A member of the group */
export interface DeviceDto {
  fingerprint: string;
  name: string;
  platform: string;
  online: boolean;
  local: boolean;
  /** Number of the Ctrl+Alt+n hotkey; null until placed */
  number: number | null;
  displays: number;
  /** Primary display in the device's own units, e.g. 1920×1080 */
  resolution: string;
  /** Display scale in percent */
  scale: number;
  swap: boolean;
  /** Pointer speed while controlled, in percent */
  pointerSpeed: number;
  /** Where it sits on the layout canvas (logical pixels): its displays' bounds */
  rect: RectDto | null;
  /** Its origin on the canvas, which placing moves */
  origin: PointDto | null;
  screens: RectDto[];
}

/** A point on the layout canvas */
export interface PointDto {
  x: number;
  y: number;
}

/** A rectangle on the layout canvas */
export interface RectDto {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** A device on the LAN outside this device's group */
export interface NearbyDto {
  fingerprint: string;
  name: string;
  platform: string;
  address: string | null;
  /** ID of the group it is in */
  group: string | null;
}

/** Where input goes right now */
export type ControlMode = "idle" | "controlling" | "controlled";

/** Who controls what */
export interface ControlDto {
  mode: ControlMode;
  /** The other device concerned: its name */
  peer: string | null;
  peerFingerprint: string | null;
  paused: boolean;
  locked: boolean;
  /** The device controlling this one locked the pointer to it */
  peerLocked: boolean;
  /** Where the pointer is, as far as this device can tell: another
   * device's fingerprint, null for this one */
  pointer: string | null;
}

/** Why capture or injection does not run (null: it runs) */
export interface InputDto {
  capture: string | null;
  injection: string | null;
}

/** The OS input permissions */
export interface PermissionsDto {
  /** This platform has such permissions (macOS) */
  required: boolean;
  accessibility: boolean;
  inputMonitoring: boolean;
}

/** A device asking to join through this one */
export interface JoinPromptDto {
  name: string;
  platform: string;
  address: string | null;
  pin: string;
  attemptsLeft: number;
}

/** A join this device started */
export interface JoinStartDto {
  sponsor: string;
  attemptsLeft: number;
}

/** The outcome of one PIN typed */
export interface JoinAnswerDto {
  joined: boolean;
  attemptsLeft: number;
}

/** The sponsor ended the join this device asked for */
export interface JoiningEndedDto {
  reason: string | null;
}

/** A join this device sponsored is over */
export interface JoinEndedDto {
  name: string;
  admitted: boolean;
}

/** A failed command */
export interface CommandError {
  code: string;
  message: string;
}

/** The app's preferences */
export interface SettingsDto {
  language: "system" | "zh" | "en";
  theme: "system" | "dark" | "light";
  autostart: boolean;
  /** Light up the edge the pointer comes in by */
  edgeGlow: boolean;
  /** Say pauses, locks, jumps and lost devices mid-screen */
  hints: boolean;
  /** Dim this device's screens while it controls another */
  dim: boolean;
  /** Closing the main window hides it in the tray, or quits */
  closeWindow: "tray" | "quit";
  /** How opaque the windows' tint is over the blurred desktop, in percent */
  opacity: number;
}

/** What an on-screen hint says (overlay.rs) */
export type Hint =
  | { kind: "paused"; on: boolean; platform: string }
  | { kind: "locked"; on: boolean; name: string; platform: string }
  | { kind: "jump"; number: number | null; name: string }
  | { kind: "unresponsive"; name: string }
  | { kind: "lost"; name: string }
  | { kind: "letGo"; name: string; reason: string }
  | { kind: "stillRunning"; platform: string };

/** A side of a display */
export type Edge = "left" | "right" | "top" | "bottom";

/** What one display's overlay shows; `id` changes each time a part is shown again */
export interface SceneDto {
  identify: { id: number; number: number | null; name: string } | null;
  hint: { id: number; hint: Hint } | null;
  glow: { id: number; edge: Edge } | null;
  dim: boolean;
}

/** What the window asks the keyboard and mouse to do */
export type Action =
  | { kind: "pause" }
  | { kind: "lock" }
  | { kind: "jump"; fingerprint: string };
