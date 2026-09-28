// Dark / light theme: the settings' choice, or the system's.

import { getCurrentWindow } from "@tauri-apps/api/window";

/** Theme preference */
export type ThemePref = "system" | "dark" | "light";

/** localStorage key read by index.html before the first paint */
const STORAGE_KEY = "lanroam-theme";

/** Whether the system prefers light */
function systemLight(): boolean {
  return matchMedia("(prefers-color-scheme: light)").matches;
}

/** Apply a preference to the page and the native window chrome */
export function applyTheme(pref: ThemePref) {
  const light = pref === "light" || (pref === "system" && systemLight());
  document.documentElement.dataset.theme = light ? "light" : "dark";
  try {
    localStorage.setItem(STORAGE_KEY, pref);
  } catch {
    // Private mode: the theme still applies for this session
  }
  if ("__TAURI_INTERNALS__" in window) {
    getCurrentWindow()
      .setTheme(pref === "system" ? null : pref)
      .catch(console.error);
  }
}

/** How opaque the window's tint over the blurred desktop is, in percent
 * (only a window over a material shows it) */
export function applyOpacity(percent: number) {
  document.documentElement.style.setProperty("--alpha", String(percent / 100));
}

/** Follow the system while the preference is `system`; returns the unsubscribe */
export function followSystem(pref: ThemePref): () => void {
  const media = matchMedia("(prefers-color-scheme: light)");
  const onChange = () => pref === "system" && applyTheme("system");
  media.addEventListener("change", onChange);
  return () => media.removeEventListener("change", onChange);
}
