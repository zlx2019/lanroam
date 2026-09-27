// An on-screen overlay: shown by the backend over a whole display, clicks
// pass through it. For now it shows this device's number when the group
// identifies its screens.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import type { OverlayDto } from "../types";

/** The overlay page */
export function Overlay() {
  const [what, setWhat] = useState<OverlayDto | null>(null);
  // Each showing restarts the fade-in
  const [shown, setShown] = useState(0);

  useEffect(() => {
    // A freshly created window may have missed the event that showed it
    api
      .getOverlay()
      .then((current) => current && setWhat(current))
      .catch(console.error);
    const unlisten = listen<OverlayDto>(EVENTS.OVERLAY, (e) => {
      setWhat(e.payload);
      setShown((n) => n + 1);
    });
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, []);

  if (!what) return null;
  return (
    <div className="overlay">
      <div className="identify" key={shown}>
        <span className="identify-num">{what.number ?? "·"}</span>
        <span className="identify-name">{what.name}</span>
      </div>
    </div>
  );
}
