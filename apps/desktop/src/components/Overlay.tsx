// An on-screen overlay: shown by the backend over a whole display, clicks
// pass through it. It draws the display's scene: this device's number when
// the group identifies its screens, a hint near the bottom, the edge the
// pointer came in by, a dimmed screen, how far the files of a waiting drop
// are. Its colors are fixed, whatever the theme: it sits on top of any
// desktop.

import { useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import { altKey, useI18n, type Translate } from "../i18n";
import type { Hint, ReceivingDto, SceneDto } from "../types";
import {
  CheckIcon,
  CursorIcon,
  IncomingIcon,
  InfoIcon,
  LockIcon,
  OutIcon,
  PauseIcon,
  PlayIcon,
  WarnIcon,
} from "./icons";

/** The overlay page */
export function Overlay() {
  const [scene, setScene] = useState<SceneDto | null>(null);

  useEffect(() => {
    // A freshly created window may have missed the event that showed it;
    // an event that comes first is newer than the answer, though
    let heard = false;
    api
      .getOverlay()
      .then((current) => heard || setScene(current))
      .catch(console.error);
    const unlisten = listen<SceneDto>(EVENTS.OVERLAY_SCENE, (e) => {
      heard = true;
      setScene(e.payload);
    });
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, []);

  if (!scene) return null;
  const { identify, hint, glow, dim, receiving } = scene;
  return (
    <div className="overlay">
      {dim && <div className="ov-dim" />}
      {/* A new id remounts a part, which restarts its animation */}
      {glow && <div key={glow.id} className={`ov-glow ${glow.edge}`} />}
      {identify && (
        <div key={identify.id} className="identify">
          <span className="identify-num">{identify.number ?? "·"}</span>
          <span className="identify-name">{identify.name}</span>
        </div>
      )}
      {hint && <HintLine key={hint.id} hint={hint.hint} />}
      {receiving && <Receiving receiving={receiving} />}
    </div>
  );
}

/** Where the chip sits from the drop point, and the room it takes */
const CHIP = { offset: 18, width: 290, height: 76 };

/** The files of a waiting drop still arriving, next to where it lands */
function Receiving({ receiving }: { receiving: ReceivingDto }) {
  const { t } = useI18n();
  const { x, y, name, count, done, total } = receiving;
  const percent = total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : 0;
  const what = several(name, count, t);
  // Below and right of the drop point, kept on the display
  const left = Math.max(0, Math.min(x + CHIP.offset, window.innerWidth - CHIP.width));
  const top = Math.max(0, Math.min(y + CHIP.offset, window.innerHeight - CHIP.height));
  return (
    <div className="ov-receiving" style={{ left, top, width: CHIP.width }}>
      <IncomingIcon />
      <div className="ov-receiving-body">
        <div className="ov-receiving-line">
          <span className="ov-receiving-name">{what}</span>
          <span>{percent}%</span>
        </div>
        <div className="ov-bar">
          <div style={{ width: `${percent}%` }} />
        </div>
        <small>
          {t("drop.receiving", { done: bytes(done), total: bytes(total) })} · {t("drop.cancel")}
        </small>
      </div>
    </div>
  );
}

/** The first of `count` files by name, and how many more */
function several(name: string, count: number, t: Translate): string {
  return count > 1 ? t("drop.more", { name, count, others: count - 1 }) : name;
}

/** A size for people: 820 B, 3.4 MB, 1.2 GB (1024-based) */
function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

/** A hint: an icon, a line and maybe a smaller one, fading out on its own */
function HintLine({ hint }: { hint: Hint }) {
  const { t } = useI18n();
  const { icon, text, sub, warn } = words(hint, t);
  return (
    <div className={`ov-hint${warn ? " warn" : ""}`}>
      {icon}
      <span>{text}</span>
      {sub && <small>{sub}</small>}
    </div>
  );
}

/** How a hint reads */
interface Words {
  icon: ReactNode;
  text: string;
  sub?: string;
  warn?: boolean;
}

/** The words for `hint` */
function words(hint: Hint, t: Translate): Words {
  /** A hotkey as `platform` names it, e.g. Ctrl+Option+Esc */
  const keys = (platform: string, key: string) => `Ctrl+${altKey(platform)}+${key}`;
  switch (hint.kind) {
    case "paused":
      return hint.on
        ? { icon: <PauseIcon />, text: t("hint.paused"), sub: t("hint.pausedSub", { keys: keys(hint.platform, "Esc") }) }
        : { icon: <PlayIcon />, text: t("hint.resumed") };
    case "locked":
      return hint.on
        ? {
            icon: <LockIcon />,
            text: t("hint.locked", { name: hint.name }),
            sub: t("hint.lockedSub", { keys: keys(hint.platform, "L") }),
          }
        : { icon: <CursorIcon />, text: t("hint.unlocked") };
    case "jump":
      return { icon: <OutIcon />, text: hint.number ? `${hint.number} · ${hint.name}` : hint.name };
    case "unresponsive":
      return { icon: <WarnIcon />, text: t("hint.unresponsive", { name: hint.name }), sub: t("hint.back"), warn: true };
    case "lost":
      return { icon: <WarnIcon />, text: t("hint.lost", { name: hint.name }), sub: t("hint.back"), warn: true };
    case "letGo":
      return letGo(hint.name, hint.reason, t);
    case "dragFailed":
      return {
        icon: <WarnIcon />,
        text: t(hint.reason === "no_space" ? "hint.dragNoSpace" : "hint.dragFailed", { name: hint.name }),
        sub: t("hint.dragNothing"),
        warn: true,
      };
    case "filesReady":
      return { icon: <CheckIcon />, text: t("hint.filesReady"), sub: several(hint.name, hint.count, t) };
    case "filesTooLarge":
      return {
        icon: <InfoIcon />,
        text: t("hint.filesTooLarge", { limit: bytes(hint.limit) }),
        sub: t("hint.filesTooLargeSub", { what: several(hint.name, hint.count, t), size: bytes(hint.bytes) }),
      };
    case "filesFailed":
      return {
        icon: <WarnIcon />,
        text: t(hint.reason === "no_space" ? "hint.filesNoSpace" : "hint.filesFailed"),
        sub: several(hint.name, hint.count, t),
        warn: true,
      };
    case "stillRunning": {
      const menuBar = hint.platform === "macos";
      return {
        icon: <InfoIcon />,
        text: t(menuBar ? "hint.runningMenuBar" : "hint.runningTray"),
        sub: t(menuBar ? "hint.runningMenuBarSub" : "hint.runningTraySub"),
      };
    }
  }
}

/** The words for a device that let go, by reason (protocol::released) */
function letGo(name: string, reason: string, t: Translate): Words {
  const sub = t("hint.back");
  switch (reason) {
    case "preempted":
      return { icon: <InfoIcon />, text: t("hint.preempted", { name }), sub };
    case "local_input":
      return { icon: <InfoIcon />, text: t("hint.tookBack", { name }), sub };
    case "unavailable":
      return { icon: <WarnIcon />, text: t("hint.unavailable", { name }), sub, warn: true };
    default:
      return { icon: <InfoIcon />, text: t("hint.letGo", { name }), sub };
  }
}
