// Development only: a fake backend for the main window, so the UI can be
// tried and tested in a plain browser (served by `pnpm dev` at /mock.html,
// never part of the build). A Mac and a Windows PC side by side, one
// device nearby; placing moves devices and pushes a new snapshot.
//
// `/mock.html?window=overlay-0` is an on-screen overlay instead: push it
// scenes with `window.__emit("overlay-scene", scene)`.

import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { DeviceDto, NearbyDto, SceneDto, SettingsDto, Snapshot } from "../types";

/** A device with one display of `w`×`h` (device units) at `scale` percent */
function device(
  fingerprint: string,
  name: string,
  platform: string,
  w: number,
  h: number,
  scale: number,
  x: number,
  local = false,
): DeviceDto {
  const lw = (w * 100) / scale;
  const lh = (h * 100) / scale;
  return {
    fingerprint,
    name,
    platform,
    online: true,
    local,
    number: null,
    displays: 1,
    resolution: `${w}×${h}`,
    scale,
    swap: true,
    rect: { x, y: 0, w: lw, h: lh },
    screens: [{ x, y: 0, w: lw, h: lh }],
    origin: { x, y: 0 },
  };
}

const state: Snapshot = {
  device: { fingerprint: "mac".padEnd(64, "0"), name: "ZeroMac-mini", platform: "macos", version: "0.1.0" },
  group: {
    id: "651d9065-7994-422d-832b-02aaab7378ee",
    devices: [
      device("mac".padEnd(64, "0"), "ZeroMac-mini", "macos", 2560, 1440, 100, 0, true),
      device("pc".padEnd(64, "0"), "DESKTOP-LBKSIT1", "windows", 1920, 1080, 125, 2560),
    ],
  },
  control: { mode: "idle", peer: null, peerFingerprint: null, paused: false, locked: false },
  input: { capture: null, injection: null },
};

const nearby: NearbyDto[] = [
  { fingerprint: "office".padEnd(64, "0"), name: "OFFICE-PC", platform: "windows", address: "192.168.1.57", group: null },
];

let settings: SettingsDto = {
  language: "zh",
  theme: "dark",
  autostart: true,
  edgeGlow: true,
  hints: true,
  dim: false,
};

/** What an overlay window shows at first */
const scene: SceneDto = { identify: null, hint: null, glow: null, dim: false };

/** Numbers in reading order, as the engine assigns them */
function renumber() {
  const devices = state.group?.devices ?? [];
  const order = [...devices].sort((a, b) => (a.origin?.x ?? 0) - (b.origin?.x ?? 0) || (a.origin?.y ?? 0) - (b.origin?.y ?? 0));
  for (const d of devices) d.number = order.indexOf(d) + 1;
}
renumber();

/** Commands the mock answers; the rest resolve to nothing */
const commands: Record<string, (args: Record<string, unknown>) => unknown> = {
  get_snapshot: () => state,
  list_nearby: () => nearby,
  get_permissions: () => ({ required: true, accessibility: true, inputMonitoring: true }),
  get_settings: () => settings,
  get_overlay: () => scene,
  save_settings: (args) => {
    settings = args.settings as SettingsDto;
  },
  place: (args) => {
    const d = state.group?.devices.find((x) => x.fingerprint === args.fingerprint);
    if (!d?.rect || !d.origin) return;
    const dx = (args.x as number) - d.origin.x;
    const dy = (args.y as number) - d.origin.y;
    d.origin = { x: args.x as number, y: args.y as number };
    d.rect = { ...d.rect, x: d.rect.x + dx, y: d.rect.y + dy };
    renumber();
    // The engine pushes the new state once the group has it
    setTimeout(() => emit("snapshot", structuredClone(state)), 50);
  },
};

mockWindows(new URLSearchParams(location.search).get("window") ?? "main");
mockIPC(
  (cmd, payload) => {
    (window as unknown as { __calls: unknown[] }).__calls.push({ cmd, payload });
    return commands[cmd]?.((payload ?? {}) as Record<string, unknown>);
  },
  { shouldMockEvents: true },
);
(window as unknown as { __calls: unknown[] }).__calls = [];
(window as unknown as { __emit: typeof emit }).__emit = emit;

await import("../main");
