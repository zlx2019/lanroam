// The main window: devices, their screens' arrangement and settings, with
// the join dialog and (on macOS, until granted) the permission walkthrough.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { EVENTS } from "./events";
import { useNearby, useSnapshot, useToast } from "./hooks/useLanroam";
import { useI18n } from "./i18n";
import type { NearbyDto, PermissionsDto, SettingsDto } from "./types";
import { ArrangePage } from "./components/ArrangePage";
import { Header, type Tab } from "./components/Chrome";
import { DevicesPage } from "./components/DevicesPage";
import { JoinModal } from "./components/JoinModal";
import { Onboarding } from "./components/Onboarding";
import { SettingsPage, type Section } from "./components/SettingsPage";

/** The main window */
export default function App({
  settings,
  onSettings,
}: {
  settings: SettingsDto;
  onSettings: (next: SettingsDto) => Promise<void>;
}) {
  const { t } = useI18n();
  const snapshot = useSnapshot();
  const [tab, setTab] = useState<Tab>("devices");
  const [section, setSection] = useState<Section>("general");
  // Came to the settings to rename this device
  const [renaming, setRenaming] = useState(false);
  const [toast, showToast] = useToast();
  const [joinTarget, setJoinTarget] = useState<NearbyDto | null>(null);
  const [perms, setPerms] = useState<PermissionsDto | null>(null);
  const [permsDone, setPermsDone] = useState(false);
  const nearby = useNearby(tab === "devices");

  useEffect(() => {
    api.getPermissions().then(setPerms).catch(console.error);
  }, []);

  useEffect(() => {
    const unlisten = listen(EVENTS.KICKED, () => showToast(t("toast.kicked")));
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, [showToast, t]);

  // The tray opens the window on a page
  useEffect(() => {
    const unlisten = listen<Tab>(EVENTS.SHOW_PAGE, (e) => {
      setRenaming(false);
      setTab(e.payload);
    });
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, []);

  if (!snapshot || !perms) return <div className="app" />;

  const needPerms =
    perms.required && !(perms.accessibility && perms.inputMonitoring) && !permsDone;
  if (needPerms) {
    return (
      <div className="app">
        <Header snapshot={snapshot} />
        <main>
          <Onboarding onDone={() => setPermsDone(true)} />
        </main>
      </div>
    );
  }

  /** Show a page, as a click on its tab */
  const show = (next: Tab) => {
    setRenaming(false);
    setTab(next);
  };

  return (
    <div className="app">
      <Header snapshot={snapshot} tab={tab} onTab={show} onToast={showToast} />
      <main>
        {tab === "devices" && (
          <DevicesPage
            snapshot={snapshot}
            nearby={nearby}
            onJoin={setJoinTarget}
            onRename={() => {
              setSection("general");
              setRenaming(true);
              setTab("settings");
            }}
            onToast={showToast}
          />
        )}
        {tab === "arrange" && <ArrangePage snapshot={snapshot} />}
        {tab === "settings" && (
          <SettingsPage
            snapshot={snapshot}
            settings={settings}
            section={section}
            renaming={renaming}
            onSection={(next) => {
              setRenaming(false);
              setSection(next);
            }}
            onSettings={onSettings}
            onToast={showToast}
          />
        )}
      </main>
      {joinTarget && (
        <JoinModal
          target={joinTarget}
          grouped={!!snapshot.group}
          onClose={() => setJoinTarget(null)}
          onJoined={() => {
            setJoinTarget(null);
            // Straight on to placing the screens
            show("arrange");
          }}
        />
      )}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
