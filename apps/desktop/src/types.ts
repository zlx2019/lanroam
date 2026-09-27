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
  peer: string | null;
  paused: boolean;
  locked: boolean;
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
}

/** What the on-screen overlays show */
export type OverlayDto = { kind: "identify"; number: number | null; name: string };

/** What the window asks the keyboard and mouse to do */
export type Action =
  | { kind: "pause" }
  | { kind: "lock" }
  | { kind: "jump"; fingerprint: string };
