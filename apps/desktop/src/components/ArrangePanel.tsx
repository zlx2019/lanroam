// The arrangement panel: its own frameless window floating mid-screen over
// a material (panel.rs), with the group's screens to drag into place. Every
// drop is saved at once. Esc or "Done" hides the window, and so does a
// click anywhere else (the backend hides it when it loses focus).

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import { clash, fit, pairs, seams, shift, snap, type Placed, type Seam, type View } from "../geometry";
import { useSnapshot } from "../hooks/useLanroam";
import { altKey, formatError, useI18n, type Translate } from "../i18n";
import type { DeviceDto, GroupDto, Snapshot } from "../types";
import { WarnIcon } from "./icons";
import { Screens } from "./Screens";

/** Room around the screens when fitting them in the well (px) */
const FIT_PADDING = 34;

/** Largest scale: a lone device must not fill the whole well */
const MAX_SCALE = 0.14;

/** How close a dragged edge snaps to another screen's, in screen pixels */
const SNAP_PX = 12;

/** Pointer travel before a press counts as a drag, in screen pixels */
const DRAG_PX = 3;

/** How long a refused drop slides back (matches the .dev transition) */
const BOUNCE_MS = 280;

/** How long a placed device waits for the group to confirm its spot */
const PENDING_MS = 3000;

/** A line in the foot, from the last drop; `warn` shows it as a warning */
interface Note {
  text: string;
  warn: boolean;
}

/** Hide the panel's window */
function hide() {
  getCurrentWindow().hide().catch(console.error);
}

/** The panel */
export function ArrangePanel() {
  const { t } = useI18n();
  const snapshot = useSnapshot();
  const [note, setNote] = useState<Note | null>(null);
  // The device dropped last, told about when it touches no other
  const [dropped, setDropped] = useState<string | null>(null);

  // Esc hides; each showing starts afresh, without the last one's notes
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && hide();
    window.addEventListener("keydown", esc);
    const unlisten = getCurrentWindow().onFocusChanged(({ payload }) => {
      if (!payload) return;
      setNote(null);
      setDropped(null);
    });
    return () => {
      window.removeEventListener("keydown", esc);
      unlisten.then((u) => u()).catch(console.error);
    };
  }, []);

  if (!snapshot) return <div className="panel" />;
  const foot = note ?? standing(snapshot, dropped, t) ?? { text: t("panel.hint"), warn: false };
  return (
    <div className="panel">
      <div className="p-head">
        <b>{t("panel.title")}</b>
        <span>{t("panel.subtitle")}</span>
        <button className="done" onClick={hide}>
          {t("panel.done")} <span className="kbd">Esc</span>
        </button>
      </div>
      {snapshot.group ? (
        <Well
          group={snapshot.group}
          onNote={setNote}
          onDrop={(id) => {
            setNote(null);
            setDropped(id);
          }}
        />
      ) : (
        <div className="well empty">{t("panel.noGroup")}</div>
      )}
      <div className={`p-foot${foot.warn ? " warn" : ""}`}>
        {foot.warn && <WarnIcon />}
        {foot.text}
      </div>
    </div>
  );
}

/** What the foot says while nothing new happened: a device out of reach
 * (the one dropped last, or any online one), or devices without screens */
function standing(snapshot: Snapshot, dropped: string | null, t: Translate): Note | null {
  const devices = snapshot.group?.devices ?? [];
  const placed = devices.filter((d) => d.screens.length);
  const lonely = lonelyDevices(placed);
  const shown = lonely.find((d) => d.fingerprint === dropped) ?? lonely.find((d) => d.online);
  if (shown) return { text: lonelyText(shown, snapshot, t), warn: true };
  const unplaced = devices.length - placed.length;
  if (unplaced > 0) return { text: t("panel.unplaced", { n: unplaced }), warn: false };
  return null;
}

/** The devices touching no other, when there are others to touch */
function lonelyDevices(placed: DeviceDto[]): DeviceDto[] {
  if (placed.length < 2) return [];
  const all = seams(placed.map((d) => ({ id: d.fingerprint, screens: d.screens })));
  return placed.filter((d) => !all.some((s) => s.a === d.fingerprint || s.b === d.fingerprint));
}

/** Says `device` is reached by its hotkey only */
function lonelyText(device: DeviceDto, snapshot: Snapshot, t: Translate): string {
  const keys = `Ctrl+${altKey(snapshot.device.platform)}+${device.number ?? ""}`;
  return t("panel.lonely", { name: device.name, keys });
}

/** A drag in progress */
interface Drag {
  /** The device */
  id: string;
  /** Where the pointer went down (screen) */
  sx: number;
  sy: number;
  /** Where the device's box started (canvas) */
  x0: number;
  y0: number;
  /** Past the drag threshold */
  moved: boolean;
}

/** A device shown somewhere else than the snapshot says: being dragged,
 * sliding back, or placed and waiting for the group to confirm. (`x`,
 * `y`) is where its box goes */
interface Moved {
  id: string;
  x: number;
  y: number;
  /** Overlaps another device */
  bad: boolean;
  /** Follows the pointer (no transition) */
  dragging: boolean;
}

/** Snap guides, in canvas units */
interface Guides {
  x: number | null;
  y: number | null;
}

/** The well: the group's screens fitted to its size, dragged into place */
function Well({
  group,
  onNote,
  onDrop,
}: {
  group: GroupDto;
  /** A drop went wrong */
  onNote: (note: Note) => void;
  /** A device was placed */
  onDrop: (id: string) => void;
}) {
  const { t } = useI18n();
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [moved, setMoved] = useState<Moved | null>(null);
  const [guides, setGuides] = useState<Guides>({ x: null, y: null });
  const drag = useRef<Drag | null>(null);
  // The latest drag position, for a drop that arrives before React has
  // rendered the last move
  const latest = useRef<Moved | null>(null);
  // The view holds still while dragging, or the well would rescale under
  // the pointer
  const frozen = useRef<View | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const devices = useMemo(() => group.devices.filter((d) => d.rect && d.screens.length), [group]);

  // A placed device waits for the snapshot that moves it (or gives up)
  useEffect(() => {
    if (!moved || moved.dragging || moved.bad) return;
    const rect = devices.find((d) => d.fingerprint === moved.id)?.rect;
    if (rect && Math.abs(rect.x - moved.x) < 1 && Math.abs(rect.y - moved.y) < 1) {
      setMoved(null);
      return;
    }
    const timer = setTimeout(() => setMoved(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [devices, moved]);

  /** Each device's screens where they show now */
  const placed: Placed[] = devices.map((d) => {
    const m = moved?.id === d.fingerprint ? moved : null;
    const rect = d.rect ?? { x: 0, y: 0 };
    return { id: d.fingerprint, screens: m ? shift(d.screens, m.x - rect.x, m.y - rect.y) : d.screens };
  });
  const view = frozen.current ?? fit(placed.flatMap((p) => p.screens), size.w, size.h, FIT_PADDING, MAX_SCALE);

  /** Start pressing a device */
  const down = (e: PointerEvent<HTMLDivElement>, id: string) => {
    if (e.button !== 0) return;
    const rect = devices.find((d) => d.fingerprint === id)?.rect;
    if (!rect) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { id, sx: e.clientX, sy: e.clientY, x0: rect.x, y0: rect.y, moved: false };
  };

  /** Follow the pointer, snapping to the other devices' screens */
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || !view) return;
    const dx = e.clientX - d.sx;
    const dy = e.clientY - d.sy;
    if (!d.moved && Math.hypot(dx, dy) < DRAG_PX) return;
    if (!d.moved) {
      d.moved = true;
      frozen.current = view;
    }
    const device = devices.find((x) => x.fingerprint === d.id);
    if (!device?.rect) return;
    const others = placed.filter((p) => p.id !== d.id).flatMap((p) => p.screens);
    const at = snap(device.rect, d.x0 + dx / view.k, d.y0 + dy / view.k, others, SNAP_PX / view.k);
    const mine = shift(device.screens, at.x - device.rect.x, at.y - device.rect.y);
    latest.current = { id: d.id, x: at.x, y: at.y, bad: clash(mine, others), dragging: true };
    setMoved(latest.current);
    setGuides({ x: at.gx, y: at.gy });
  };

  /** Drop: place the device, or slide it back when it overlaps */
  const up = () => {
    const d = drag.current;
    drag.current = null;
    setGuides({ x: null, y: null });
    if (!d?.moved) return;
    frozen.current = null;
    const device = devices.find((x) => x.fingerprint === d.id);
    const last = latest.current;
    latest.current = null;
    if (!last || !device?.origin || !device.rect || last.id !== d.id) {
      setMoved(null);
      return;
    }
    if (last.bad) {
      setMoved({ ...last, x: d.x0, y: d.y0, bad: false, dragging: false });
      setTimeout(() => setMoved(null), BOUNCE_MS);
      onNote({ text: t("panel.overlap"), warn: true });
      return;
    }
    const dx = Math.round(last.x - d.x0);
    const dy = Math.round(last.y - d.y0);
    if (!dx && !dy) {
      setMoved(null);
      return;
    }
    setMoved({ ...last, dragging: false });
    onDrop(d.id);
    api.place(d.id, device.origin.x + dx, device.origin.y + dy).catch((e) => {
      setMoved(null);
      onNote({ text: formatError(t, e), warn: true });
    });
  };

  const dragging = moved?.dragging ?? false;
  return (
    <div ref={ref} className={`well${dragging ? " dragging" : ""}`}>
      {view &&
        devices.map((d) => {
          const mine = moved?.id === d.fingerprint;
          const cls = [mine && moved.dragging && "held", mine && moved.bad && "bad"].filter(Boolean).join(" ");
          return (
            <Screens
              key={d.fingerprint}
              device={d}
              screens={placed.find((p) => p.id === d.fingerprint)?.screens ?? d.screens}
              view={view}
              labels
              className={cls}
              drag={{
                onPointerDown: (e) => down(e, d.fingerprint),
                onPointerMove: move,
                onPointerUp: up,
                onPointerCancel: up,
              }}
            />
          );
        })}
      {view && <Seams all={seams(placed)} group={group} view={view} />}
      {view && guides.x !== null && <i className="guide v" style={{ left: view.ox + guides.x * view.k }} />}
      {view && guides.y !== null && <i className="guide h" style={{ top: view.oy + guides.y * view.k }} />}
    </div>
  );
}

/** The glowing seams where devices meet; dim where the pointer cannot
 * cross (the edge is closed, or a device is offline) */
function Seams({ all, group, view }: { all: Seam[]; group: GroupDto; view: View }) {
  const open = new Map(pairs(all).map((s) => [pairKey(s.a, s.b), crossable(group, s)]));
  return (
    <>
      {all.map((s, i) => {
        const off = !open.get(pairKey(s.a, s.b));
        const along = (s.to - s.from) * view.k - 8;
        const style =
          s.line === "v"
            ? { left: view.ox + s.at * view.k - 1, top: view.oy + s.from * view.k + 4, height: along }
            : { top: view.oy + s.at * view.k - 1, left: view.ox + s.from * view.k + 4, width: along };
        return <i key={i} className={`seam ${s.line}${off ? " off" : ""}`} style={style} />;
      })}
    </>
  );
}

/** A pair's key, whatever the order */
function pairKey(a: string, b: string): string {
  return [a, b].sort().join("|");
}

/** Whether the pointer crosses between the two devices of `seam` */
function crossable(group: GroupDto, seam: Seam): boolean {
  const online = (id: string) => group.devices.find((d) => d.fingerprint === id)?.online ?? false;
  const own = group.edges.find((e) => pairKey(e.a, e.b) === pairKey(seam.a, seam.b));
  return (own?.crossable ?? true) && online(seam.a) && online(seam.b);
}
