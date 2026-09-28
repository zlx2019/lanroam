// The main window's top bar: the brand, the pages, the state (which is the
// pause switch too) and the way into the arrangement panel.

import { useI18n, type Translate } from "../i18n";
import { api } from "../api";
import type { Snapshot } from "../types";
import { ArrangeIcon, LockIcon, Logo } from "./icons";

/** Pages of the main window */
export type Tab = "devices" | "settings";

/** Pages in tab order */
const TABS: Tab[] = ["devices", "settings"];

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
  return (
    <header className={`top${mac ? " mac" : ""}`} data-tauri-drag-region>
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
            {snapshot.group && (
              <button className="btn" onClick={() => api.showPanel().catch(console.error)}>
                <ArrangeIcon />
                {t("action.arrange")}
              </button>
            )}
          </div>
        </>
      )}
    </header>
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
