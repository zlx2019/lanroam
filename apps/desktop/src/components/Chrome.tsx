// The main window's top bar: the brand, the pages and the state (which is
// the pause switch too). It is the window's title bar as well: on macOS
// under the traffic lights, on Windows with window buttons of its own.

import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useI18n, type Translate } from "../i18n";
import { api } from "../api";
import type { Snapshot } from "../types";
import { CloseIcon, LockIcon, Logo, MaximizeIcon, MinimizeIcon, RestoreIcon } from "./icons";

/** Pages of the main window */
export type Tab = "devices" | "arrange" | "settings";

/** Pages in tab order */
const TABS: Tab[] = ["devices", "arrange", "settings"];

/** Where sharing stands: working, paused, or nothing to share with (no
 * group, or no other member online); the tray says the same (tray.rs) */
export type Activity = "active" | "paused" | "inactive";

/** Where sharing stands in `snapshot` */
export function activity(snapshot: Snapshot): Activity {
  const others = snapshot.group?.devices.some((d) => !d.local && d.online) ?? false;
  if (!others) return "inactive";
  return snapshot.control.paused ? "paused" : "active";
}

/** The top bar; without `tab` it shows the brand only (onboarding) */
export function Header({
  snapshot,
  tab,
  onTab,
  onToast,
}: {
  snapshot: Snapshot;
  tab?: Tab;
  onTab?: (tab: Tab) => void;
  onToast?: (message: string) => void;
}) {
  const { t } = useI18n();
  const mac = snapshot.device.platform === "macos";
  const windows = snapshot.device.platform === "windows";
  return (
    <header className={`top${mac ? " mac" : ""}${windows ? " win" : ""}`} data-tauri-drag-region>
      <span className="brand" data-tauri-drag-region>
        <Logo />
        Lanroam
      </span>
      {tab && onTab && (
        <>
          <nav className="seg tabs">
            {TABS.map((id) => (
              <button key={id} className={tab === id ? "on" : ""} onClick={() => onTab(id)}>
                {t(`tab.${id}`)}
              </button>
            ))}
          </nav>
          <div className="right" data-tauri-drag-region>
            <StateIndicator snapshot={snapshot} onToast={onToast} />
          </div>
        </>
      )}
      {windows && <WindowButtons />}
    </header>
  );
}

/** Minimize, maximize or restore, and close (Windows, where the window has
 * no title bar of its own); closing does what closing the window does */
function WindowButtons() {
  const { t } = useI18n();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const current = getCurrentWindow();
    const update = () => current.isMaximized().then(setMaximized).catch(console.error);
    update();
    const unlisten = current.onResized(update);
    return () => {
      unlisten.then((stop) => stop()).catch(console.error);
    };
  }, []);

  const current = getCurrentWindow();
  const run = (action: Promise<void>) => action.catch(console.error);
  return (
    <div className="win-buttons">
      <button aria-label={t("window.minimize")} onClick={() => run(current.minimize())}>
        <MinimizeIcon />
      </button>
      <button
        aria-label={t(maximized ? "window.restore" : "window.maximize")}
        onClick={() => run(current.toggleMaximize())}
      >
        {maximized ? <RestoreIcon /> : <MaximizeIcon />}
      </button>
      <button className="close" aria-label={t("window.close")} onClick={() => run(current.close())}>
        <CloseIcon />
      </button>
    </div>
  );
}

/** The state in a word, the detail on hover; a click pauses or resumes,
 * like the pause hotkey */
function StateIndicator({ snapshot, onToast }: { snapshot: Snapshot; onToast?: (message: string) => void }) {
  const { t } = useI18n();
  const state = activity(snapshot);
  const { control } = snapshot;
  const locked = control.locked || control.peerLocked;

  /** Pause or resume at the next local input */
  const toggle = () => {
    if (state === "inactive") return;
    api
      .requestAction({ kind: "pause" })
      .then(() => onToast?.(t(control.paused ? "toast.resumed" : "toast.paused")))
      .catch(console.error);
  };

  const dot = state === "active" ? "live" : state === "paused" ? "warn" : "";
  return (
    <button className={`state${state === "inactive" ? " idle" : ""}`} onClick={toggle}>
      <span className={`dot ${dot}`} />
      {t(`state.${state}`)}
      {locked && state === "active" && (
        <span className="lk">
          <LockIcon />
        </span>
      )}
      <span className="tip">{detail(snapshot, state, t)}</span>
    </button>
  );
}

/** What the state's tooltip says: the detail behind it, and what a click
 * does */
function detail(snapshot: Snapshot, state: Activity, t: Translate): string {
  const { control, group } = snapshot;
  if (!group) return t("state.noGroup");
  if (state === "inactive") return t("state.alone");
  if (state === "paused") return t("state.clickResume");
  const name = control.peer ?? "";
  const where =
    control.mode === "controlling"
      ? t(control.locked ? "state.locked" : "state.controlling", { name })
      : control.mode === "controlled"
        ? t("state.controlled", { name })
        : t("state.here");
  return `${where} · ${t("state.clickPause")}`;
}
