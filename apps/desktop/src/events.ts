/** Event names, mirrored from the events module in src-tauri/src/bridge.rs */
export const EVENTS = {
  /** The whole state changed; payload: Snapshot */
  SNAPSHOT: "snapshot",
  /** Someone asks to join through this device; payload: JoinPromptDto */
  JOIN_PROMPT: "join-prompt",
  /** The join this device sponsored is over; payload: JoinEndedDto */
  JOIN_ENDED: "join-ended",
  /** The sponsor ended the join this device asked for; payload: JoiningEndedDto */
  JOINING_ENDED: "joining-ended",
  /** Another member removed this device from the group */
  KICKED: "kicked",
  /** What one on-screen overlay shows, sent to that window; payload: SceneDto (overlay.rs) */
  OVERLAY_SCENE: "overlay-scene",
} as const;
