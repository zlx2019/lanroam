// Typed wrappers of the commands in src-tauri/src/commands.rs.

import { invoke } from "@tauri-apps/api/core";
import type {
  Action,
  EdgeSettings,
  InputDto,
  InputSettings,
  JoinAnswerDto,
  JoinPromptDto,
  JoinStartDto,
  NearbyDto,
  SceneDto,
  PermissionsDto,
  SettingsDto,
  Snapshot,
} from "./types";

export const api = {
  /** The whole state */
  getSnapshot: () => invoke<Snapshot>("get_snapshot"),
  /** Devices on the LAN outside this device's group */
  listNearby: () => invoke<NearbyDto[]>("list_nearby"),
  /** Ask a device to let this one in; resolves once it shows its PIN */
  startJoin: (fingerprint: string) => invoke<JoinStartDto>("start_join", { fingerprint }),
  /** Answer the join in progress with a PIN */
  answerJoin: (pin: string) => invoke<JoinAnswerDto>("answer_join", { pin }),
  /** Give up the join in progress */
  cancelJoin: () => invoke<void>("cancel_join"),
  /** The join this device sponsors right now */
  getJoinPrompt: () => invoke<JoinPromptDto | null>("get_join_prompt"),
  /** Turn down the join this device sponsors */
  rejectJoin: () => invoke<void>("reject_join"),
  /** Leave the desk group */
  leaveGroup: () => invoke<void>("leave_group"),
  /** Remove a member */
  kick: (fingerprint: string) => invoke<void>("kick", { fingerprint }),
  /** Move a member: its origin to (x, y) on the canvas */
  place: (fingerprint: string, x: number, y: number) =>
    invoke<void>("place", { fingerprint, x, y }),
  /** Every online member shows its number on its screens */
  identify: () => invoke<void>("identify"),
  /** What this overlay window shows right now */
  getOverlay: () => invoke<SceneDto>("get_overlay"),
  /** Rename this device */
  rename: (name: string) => invoke<void>("rename", { name }),
  /** Swap Command and Control for input into this device */
  setSwap: (on: boolean) => invoke<void>("set_swap", { on }),
  /** Pause, lock or jump at the next local input */
  requestAction: (action: Action) => invoke<void>("request_action", { action }),
  /** The OS input permissions */
  getPermissions: () => invoke<PermissionsDto>("get_permissions"),
  /** Open the system settings for a permission */
  openPermission: (permission: "accessibility" | "inputMonitoring") =>
    invoke<void>("open_permission", { permission }),
  /** Start capture and injection if they do not run yet */
  restartInput: () => invoke<InputDto>("restart_input"),
  /** Start Lanroam over */
  relaunch: () => invoke<void>("relaunch"),
  /** The app's preferences */
  getSettings: () => invoke<SettingsDto>("get_settings"),
  /** Save the app's preferences */
  saveSettings: (settings: SettingsDto) => invoke<void>("save_settings", { settings }),
  /** This device's input settings */
  getInputSettings: () => invoke<InputSettings>("get_input_settings"),
  /** Use and save new input settings */
  saveInputSettings: (settings: InputSettings) => invoke<void>("save_input_settings", { settings }),
  /** Record the next key combination (the `recorded` event), or stop */
  recordKeys: (on: boolean) => invoke<void>("record_keys", { on }),
  /** Set the edge between two members, for the whole group */
  setEdge: (a: string, b: string, settings: EdgeSettings) => invoke<void>("set_edge", { a, b, settings }),
  /** Key names by HID usage (W3C code values) */
  keyNames: () => invoke<Record<string, string>>("key_names"),
  /** Quit Lanroam */
  quit: () => invoke<void>("quit_app"),
};
