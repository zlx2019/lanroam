// The main window: layout, devices and settings, with the join dialog and
// (on macOS, until granted) the permission walkthrough.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./api";
import { EVENTS } from "./events";
import { useNearby, useSnapshot, useToast } from "./hooks/useLanroam";
import { useI18n } from "./i18n";
import type { NearbyDto, PermissionsDto, SettingsDto } from "./types";
import { Footer, Header, type Tab } from "./components/Chrome";
import { DevicesPage } from "./components/DevicesPage";
import { JoinModal } from "./components/JoinModal";
import { LayoutPage } from "./components/LayoutPage";
import { Onboarding } from "./components/Onboarding";
import { SettingsPage } from "./components/SettingsPage";

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
  const [tab, setTab] = useState<Tab>("layout");
  const [toast, showToast] = useToast();
  const [joinTarget, setJoinTarget] = useState<NearbyDto | null>(null);
  const [joined, setJoined] = useState(false);
  const [perms, setPerms] = useState<PermissionsDto | null>(null);
  const [permsDone, setPermsDone] = useState(false);
  const nearby = useNearby(tab === "devices" || (snapshot !== null && !snapshot.group));

  useEffect(() => {
    api.getPermissions().then(setPerms).catch(console.error);
  }, []);

  useEffect(() => {
    const unlisten = listen(EVENTS.KICKED, () => showToast(t("toast.kicked")));
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, [showToast, t]);

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
        <Footer snapshot={snapshot} nearby={nearby.length} />
      </div>
    );
  }

  return (
    <div className="app">
      <Header snapshot={snapshot} tab={tab} onTab={setTab} onToast={showToast} />
      <main>
        {tab === "layout" && (
          <LayoutPage
            snapshot={snapshot}
            nearby={nearby}
            joined={joined}
            onJoin={setJoinTarget}
            onJoinedSeen={() => setJoined(false)}
            onToast={showToast}
          />
        )}
        {tab === "devices" && (
          <DevicesPage snapshot={snapshot} nearby={nearby} onJoin={setJoinTarget} onToast={showToast} />
        )}
        {tab === "settings" && (
          <SettingsPage
            snapshot={snapshot}
            settings={settings}
            onSettings={onSettings}
            onToast={showToast}
          />
        )}
      </main>
      <Footer snapshot={snapshot} nearby={nearby.length} />
      {joinTarget && (
        <JoinModal
          target={joinTarget}
          grouped={!!snapshot.group}
          onClose={() => setJoinTarget(null)}
          onJoined={() => {
            setJoinTarget(null);
            setJoined(true);
            setTab("layout");
          }}
        />
      )}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
