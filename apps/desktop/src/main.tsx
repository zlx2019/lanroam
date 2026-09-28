// Entry: one bundle for every window, routed by window label (main: the
// app, arrange: the arrangement panel, join: the PIN shown to a device
// asking to join, overlay-n: what shows over display n).

import React, { Suspense, lazy, useCallback, useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "./api";
import { I18nProvider, resolveLang } from "./i18n";
import { applyTheme, followSystem } from "./theme";
import type { SettingsDto } from "./types";
import "./index.css";

const App = lazy(() => import("./App"));
const JoinWindow = lazy(() =>
  import("./components/JoinWindow").then((m) => ({ default: m.JoinWindow })),
);
const Overlay = lazy(() => import("./components/Overlay").then((m) => ({ default: m.Overlay })));
const ArrangePanel = lazy(() =>
  import("./components/ArrangePanel").then((m) => ({ default: m.ArrangePanel })),
);

/** This window's label */
const label = getCurrentWindow().label;

/** An on-screen overlay: a transparent page over a whole display */
const isOverlay = label.startsWith("overlay-");
if (isOverlay) document.documentElement.dataset.overlay = "1";

// Styles that differ by system (window corners, fonts)
document.documentElement.dataset.os = navigator.userAgent.includes("Windows") ? "windows" : "macos";

/** Loads the preferences, then the window's content in their language */
function Root() {
  const [settings, setSettings] = useState<SettingsDto | null>(null);

  useEffect(() => {
    api
      .getSettings()
      .then((s) => {
        setSettings(s);
        applyTheme(s.theme);
      })
      .catch(console.error);
  }, []);

  const theme = settings?.theme;
  useEffect(() => (theme ? followSystem(theme) : undefined), [theme]);

  /** Save and apply new preferences */
  const save = useCallback(async (next: SettingsDto) => {
    await api.saveSettings(next);
    setSettings(next);
    applyTheme(next.theme);
  }, []);

  if (!settings) return null;
  return (
    <I18nProvider lang={resolveLang(settings.language)}>
      <Suspense fallback={null}>
        {isOverlay ? (
          <Overlay />
        ) : label === "join" ? (
          <JoinWindow />
        ) : label === "arrange" ? (
          <ArrangePanel />
        ) : (
          <App settings={settings} onSettings={save} />
        )}
      </Suspense>
    </I18nProvider>
  );
}

// Outside development, the WebView's own context menu (Reload, ...) stays
// off; text fields keep copy and paste
if (!import.meta.env.DEV) {
  document.addEventListener("contextmenu", (e) => {
    const el = e.target instanceof Element ? e.target : null;
    if (!el?.closest("input, textarea")) e.preventDefault();
  });
}

const root = document.getElementById("root");
if (root) {
  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <Root />
    </React.StrictMode>,
  );
}
