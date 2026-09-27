// The main window's frame: header (tabs, where input is, pause) and footer.

import { useI18n } from "../i18n";
import { api } from "../api";
import type { Snapshot } from "../types";
import { InIcon, LockIcon, Logo, OutIcon, PauseIcon, PlayIcon } from "./icons";

/** Pages of the main window */
export type Tab = "layout" | "devices" | "settings";

/** Pages in tab order */
const TABS: Tab[] = ["layout", "devices", "settings"];

/** The header; without `tab` it shows the brand only (onboarding) */
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
  const paused = snapshot.control.paused;
  /** Pause or resume at the next local input, like Ctrl+Alt+Esc */
  const togglePause = () => {
    api
      .requestAction({ kind: "pause" })
      .then(() => onToast?.(t(paused ? "toast.resumed" : "toast.paused")))
      .catch(console.error);
  };
  return (
    <header className={`top${mac ? " mac" : ""}`} data-tauri-drag-region>
      <span className="brand" data-tauri-drag-region>
        <Logo />
        Lanroam
      </span>
      {tab && onTab && (
        <>
          <nav className="tabs">
            {TABS.map((id) => (
              <button key={id} className={tab === id ? "on" : ""} onClick={() => onTab(id)}>
                {t(`tab.${id}`)}
              </button>
            ))}
          </nav>
          <div className="status" data-tauri-drag-region>
            <StatusPill snapshot={snapshot} />
            {snapshot.group && (
              <button className="btn" onClick={togglePause} title="Ctrl+Alt+Esc">
                {paused ? <PlayIcon /> : <PauseIcon />}
                {t(paused ? "action.resume" : "action.pause")}
              </button>
            )}
          </div>
        </>
      )}
    </header>
  );
}

/** Where input goes right now */
function StatusPill({ snapshot }: { snapshot: Snapshot }) {
  const { t } = useI18n();
  const { control, group } = snapshot;
  const name = control.peer ?? "";
  if (!group) {
    return (
      <div className="pill muted">
        <span className="dot grey" />
        {t("status.noGroup")}
      </div>
    );
  }
  if (control.mode === "controlling") {
    return (
      <div className="pill out">
        {control.locked ? <LockIcon /> : <OutIcon />}
        {t("status.controlling", { name })}
      </div>
    );
  }
  if (control.mode === "controlled") {
    return (
      <div className="pill in">
        <InIcon />
        {t("status.controlled", { name })}
      </div>
    );
  }
  if (control.paused) {
    return (
      <div className="pill muted">
        <PauseIcon />
        {t("status.paused")}
      </div>
    );
  }
  if (control.locked) {
    return (
      <div className="pill">
        <LockIcon />
        {t("status.locked")}
      </div>
    );
  }
  return (
    <div className="pill">
      <span className="dot" />
      {t("status.idle")}
    </div>
  );
}

/** A fingerprint as `e852 1c76 … 90ca c1` */
export function shortFingerprint(fp: string): string {
  const head = fp.slice(0, 8).replace(/(.{4})/g, "$1 ").trim();
  const tail = fp.slice(-6).replace(/(.{4})/, "$1 ");
  return `${head} … ${tail}`;
}

/** The footer: group at a glance, fingerprint, version */
export function Footer({ snapshot, nearby }: { snapshot: Snapshot; nearby: number }) {
  const { t } = useI18n();
  const { group, device } = snapshot;
  return (
    <footer>
      {group ? (
        <span>
          <span className="dot sm" />
          {t("footer.group", {
            id: group.id.slice(0, 8),
            count: group.devices.length,
            online: group.devices.filter((d) => d.online).length,
          })}
        </span>
      ) : (
        <span>{t("footer.noGroup", { n: nearby })}</span>
      )}
      <span className="sp">
        {t("footer.fingerprint")} <code>{shortFingerprint(device.fingerprint)}</code>
      </span>
      <span>v{device.version}</span>
    </footer>
  );
}
