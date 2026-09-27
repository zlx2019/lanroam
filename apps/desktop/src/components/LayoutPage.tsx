// The layout page: the group's screens on the canvas, dragged into place,
// and the selected device beside it; outside a group, the devices to join.

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent } from "react";
import { api } from "../api";
import { fit, neighbours, overlaps, snap, touches, type Box, type Touch, type View } from "../geometry";
import { useArmed } from "../hooks/useLanroam";
import { formatError, useI18n } from "../i18n";
import type { DeviceDto, NearbyDto, Snapshot } from "../types";
import {
  CursorIcon,
  InIcon,
  InfoIcon,
  LockIcon,
  Logo,
  PlatformIcon,
  ScreenIcon,
  WarnIcon,
} from "./icons";

/** Room around the devices when fitting them in the canvas (px) */
const FIT_PADDING = 70;

/** Largest scale: a lone device must not fill the whole canvas */
const MAX_SCALE = 0.2;

/** How close a dragged edge snaps to another device's, in screen pixels */
const SNAP_PX = 14;

/** Pointer travel before a press counts as a drag, in screen pixels */
const DRAG_PX = 4;

/** How long a refused drop slides back (matches the .dev transition) */
const BOUNCE_MS = 300;

/** How long a placed device waits for the group to confirm its spot */
const PENDING_MS = 3000;

/** The key the number hotkeys use with Ctrl on this platform */
export function altKey(platform: string): string {
  return platform === "macos" ? "Option" : "Alt";
}

/** The layout page */
export function LayoutPage({
  snapshot,
  nearby,
  joined,
  onJoin,
  onJoinedSeen,
  onToast,
}: {
  snapshot: Snapshot;
  nearby: NearbyDto[];
  /** Just joined: say how to arrange the screens */
  joined: boolean;
  onJoin: (target: NearbyDto) => void;
  onJoinedSeen: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const devices = snapshot.group?.devices ?? [];
  const [selected, setSelected] = useState(snapshot.device.fingerprint);
  const current = devices.find((d) => d.fingerprint === selected) ?? devices.find((d) => d.local);
  const touching = useMemo(() => touches(boxesOf(devices)), [devices]);
  const inputProblem = snapshot.input.capture
    ? t("input.captureOff", { reason: snapshot.input.capture })
    : snapshot.input.injection
      ? t("input.injectionOff", { reason: snapshot.input.injection })
      : null;
  const unplaced = devices.filter((d) => !d.rect).length;

  /** Every online member shows its number on its screens */
  const identify = () => api.identify().catch((e) => onToast(formatError(t, e)));

  return (
    <div className="layout">
      <div className="canvas">
        {snapshot.group ? (
          <Canvas
            snapshot={snapshot}
            selected={current?.fingerprint}
            onSelect={setSelected}
            onToast={onToast}
          />
        ) : (
          <LoneDevice snapshot={snapshot} />
        )}
        {inputProblem ? (
          <div className="banner warn">
            <WarnIcon />
            <span>{inputProblem}</span>
          </div>
        ) : (
          joined && (
            <div className="banner">
              <InfoIcon />
              <span>{t("layout.joined")}</span>
              <button className="btn sm ghost" onClick={onJoinedSeen}>
                {t("layout.gotIt")}
              </button>
            </div>
          )
        )}
        {snapshot.group && !inputProblem && !joined && (
          <div className="tools">
            <button className="btn sm" onClick={identify} title={t("layout.identifyHint")}>
              <ScreenIcon />
              {t("layout.identify")}
            </button>
          </div>
        )}
        {snapshot.group ? (
          <div className="legend">
            <span>
              <i />
              {t("layout.legend.edge")}
            </span>
            <span>
              <i className="wall" />
              {t("layout.legend.wall")}
            </span>
            <span>{t("layout.legend.numbers", { alt: altKey(snapshot.device.platform) })}</span>
            {unplaced > 0 && <span>{t("layout.unplaced", { n: unplaced })}</span>}
          </div>
        ) : (
          <div className="hint">{t("layout.ungrouped")}</div>
        )}
      </div>
      <aside className="side">
        {snapshot.group && current ? (
          <SidePanel snapshot={snapshot} device={current} touching={touching} onToast={onToast} />
        ) : (
          <JoinPanel nearby={nearby} onJoin={onJoin} />
        )}
      </aside>
    </div>
  );
}

/** The placed devices' boxes */
function boxesOf(devices: DeviceDto[]): Box[] {
  return devices.flatMap((d) => (d.rect ? [{ id: d.fingerprint, ...d.rect }] : []));
}

/** A drag in progress */
interface Drag {
  /** The device */
  id: string;
  /** Where the pointer went down (screen) */
  sx: number;
  sy: number;
  /** Where the box started (canvas) */
  x0: number;
  y0: number;
  /** Past the drag threshold */
  moved: boolean;
}

/** A box shown somewhere else than the snapshot says: being dragged,
 * sliding back, or placed and waiting for the group to confirm */
interface Moved {
  id: string;
  x: number;
  y: number;
  /** Overlaps another device */
  bad: boolean;
  /** Follows the pointer (no transition) */
  dragging: boolean;
}

/** The group's devices on the canvas: fitted to its size, dragged into place */
function Canvas({
  snapshot,
  selected,
  onSelect,
  onToast,
}: {
  snapshot: Snapshot;
  selected?: string;
  onSelect: (fingerprint: string) => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [moved, setMoved] = useState<Moved | null>(null);
  const drag = useRef<Drag | null>(null);
  // The latest drag position, for a drop that arrives before React has
  // rendered the last move
  const latest = useRef<Moved | null>(null);
  // The view holds still while dragging, or the canvas would rescale under
  // the pointer
  const frozen = useRef<View | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const devices = useMemo(() => snapshot.group?.devices ?? [], [snapshot.group]);
  const placed = devices.filter((d) => d.rect);
  const boxes: Box[] = boxesOf(placed).map((b) => (moved?.id === b.id ? { ...b, x: moved.x, y: moved.y } : b));

  // A placed device waits for the snapshot that moves it (or gives up)
  useEffect(() => {
    if (!moved || moved.dragging || moved.bad) return;
    const box = devices.find((d) => d.fingerprint === moved.id)?.rect;
    if (box && Math.abs(box.x - moved.x) < 1 && Math.abs(box.y - moved.y) < 1) {
      setMoved(null);
      return;
    }
    const timer = setTimeout(() => setMoved(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [devices, moved]);

  const view = frozen.current ?? fit(boxes, size.w, size.h, FIT_PADDING, MAX_SCALE);
  if (!view) return <div ref={ref} style={{ position: "absolute", inset: 0 }} />;

  /** Start pressing a device */
  const down = (e: PointerEvent<HTMLDivElement>, box: Box) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { id: box.id, sx: e.clientX, sy: e.clientY, x0: box.x, y0: box.y, moved: false };
  };

  /** Follow the pointer, snapping to the other devices */
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.sx;
    const dy = e.clientY - d.sy;
    if (!d.moved && Math.hypot(dx, dy) < DRAG_PX) return;
    if (!d.moved) {
      d.moved = true;
      frozen.current = view;
    }
    const box = boxes.find((b) => b.id === d.id);
    if (!box) return;
    const others = boxes.filter((b) => b.id !== d.id);
    const at = snap(box, d.x0 + dx / view.k, d.y0 + dy / view.k, others, SNAP_PX / view.k);
    const bad = others.some((o) => overlaps({ ...box, ...at }, o));
    latest.current = { id: d.id, ...at, bad, dragging: true };
    setMoved(latest.current);
  };

  /** Drop: place the device, or slide it back when it overlaps */
  const up = () => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    if (!d.moved) {
      onSelect(d.id);
      return;
    }
    frozen.current = null;
    onSelect(d.id);
    const device = devices.find((x) => x.fingerprint === d.id);
    const moved = latest.current;
    latest.current = null;
    if (!moved || !device?.origin || moved.id !== d.id) {
      setMoved(null);
      return;
    }
    if (moved.bad) {
      setMoved({ ...moved, x: d.x0, y: d.y0, bad: false, dragging: false });
      setTimeout(() => setMoved(null), BOUNCE_MS);
      onToast(t("layout.overlap"));
      return;
    }
    const dx = Math.round(moved.x - d.x0);
    const dy = Math.round(moved.y - d.y0);
    if (!dx && !dy) {
      setMoved(null);
      return;
    }
    setMoved({ ...moved, dragging: false });
    const online = devices.filter((x) => x.online).length;
    api
      .place(d.id, device.origin.x + dx, device.origin.y + dy)
      .then(() => onToast(t("layout.synced", { n: online })))
      .catch((e) => {
        setMoved(null);
        onToast(formatError(t, e));
      });
  };

  const px = (x: number) => view.ox + x * view.k;
  const py = (y: number) => view.oy + y * view.k;
  const dragging = moved?.dragging ?? false;
  const { control } = snapshot;

  return (
    <div ref={ref} style={{ position: "absolute", inset: 0 }}>
      {placed.map((d) => {
        const box = boxes.find((b) => b.id === d.fingerprint);
        if (!box) return null;
        const w = box.w * view.k;
        const h = box.h * view.k;
        const target = !d.local && control.peer === d.name;
        const badge =
          target && control.mode === "controlling" ? (
            <span className="badge">
              {control.locked ? <LockIcon /> : <CursorIcon />}
              {t("tag.controlledByYou")}
            </span>
          ) : target && control.mode === "controlled" ? (
            <span className="badge warn">
              <InIcon />
              {t("tag.controllingThis")}
            </span>
          ) : null;
        const mine = moved?.id === d.fingerprint;
        const cls = [
          "dev",
          !d.online && "offline",
          d.fingerprint === selected && "sel",
          mine && moved.dragging && "dragging",
          mine && moved.bad && "bad",
        ]
          .filter(Boolean)
          .join(" ");
        return (
          <div
            key={d.fingerprint}
            className={cls}
            style={{ left: px(box.x), top: py(box.y), width: w, height: h }}
            onPointerDown={(e) => down(e, box)}
            onPointerMove={move}
            onPointerUp={up}
            onPointerCancel={up}
          >
            {d.number !== null && <span className="num">{d.number}</span>}
            {d.local && <span className="chip">{t("tag.local")}</span>}
            <span className="plat">
              <PlatformIcon platform={d.platform} />
            </span>
            <span className="dname">{d.name}</span>
            {h > 70 && !(badge && h < 120) && (
              <span className="dres">
                {d.resolution}
                {d.scale !== 100 && ` · ${d.scale}%`}
              </span>
            )}
            {!d.online && <span className="offl">{t("layout.offline")}</span>}
            {badge}
          </div>
        );
      })}
      <Edges
        touching={touches(boxes)}
        online={(id) => devices.find((d) => d.fingerprint === id)?.online ?? false}
        view={view}
        dim={control.paused}
        markers={!dragging}
      />
    </div>
  );
}

/** Both whole sides of every touching pair light up: crossing maps along
 * the whole side, proportionally, not just where the two overlap */
function Edges({
  touching,
  online,
  view,
  dim,
  markers,
}: {
  touching: Touch[];
  online: (id: string) => boolean;
  view: View;
  dim: boolean;
  markers: boolean;
}) {
  const px = (x: number) => view.ox + x * view.k;
  const py = (y: number) => view.oy + y * view.k;
  return (
    <>
      {touching.map((e) => {
        const off = !online(e.a.id) || !online(e.b.id);
        const cls = `ebar${off ? " off" : ""}${dim ? " dim" : ""}`;
        const k = view.k;
        const key = `${e.a.id}|${e.b.id}|${e.dir}`;
        const mid = (e.from + e.to) / 2;
        const [jx, jy] = e.dir === "h" ? [px(e.at), py(mid)] : [px(mid), py(e.at)];
        return (
          <div key={key}>
            {e.dir === "h" ? (
              <>
                <div className={cls} style={{ left: px(e.at) - 5, top: py(e.a.y) + 3, width: 3, height: e.a.h * k - 6 }} />
                <div className={cls} style={{ left: px(e.at) + 2, top: py(e.b.y) + 3, width: 3, height: e.b.h * k - 6 }} />
              </>
            ) : (
              <>
                <div className={cls} style={{ top: py(e.at) - 5, left: px(e.a.x) + 3, height: 3, width: e.a.w * k - 6 }} />
                <div className={cls} style={{ top: py(e.at) + 2, left: px(e.b.x) + 3, height: 3, width: e.b.w * k - 6 }} />
              </>
            )}
            {markers && <span className={`jn${off ? " off" : ""}`} style={{ left: jx, top: jy }} />}
          </div>
        );
      })}
    </>
  );
}

/** This device alone, before it joins a group */
function LoneDevice({ snapshot }: { snapshot: Snapshot }) {
  const { t } = useI18n();
  return (
    <div
      className="dev sel"
      style={{ left: "50%", top: "45%", width: 280, height: 160, transform: "translate(-50%, -50%)" }}
    >
      <span className="chip">{t("tag.local")}</span>
      <span className="plat">
        <PlatformIcon platform={snapshot.device.platform} />
      </span>
      <span className="dname">{snapshot.device.name}</span>
    </div>
  );
}

/** The selected member: what it is, what it touches, what can be done */
function SidePanel({
  snapshot,
  device,
  touching,
  onToast,
}: {
  snapshot: Snapshot;
  device: DeviceDto;
  touching: Touch[];
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [armed, press] = useArmed();
  const [swap, setSwap] = useState(device.swap);
  useEffect(() => setSwap(device.swap), [device.swap]);
  const devices = snapshot.group?.devices ?? [];
  const mac = device.platform === "macos";
  const [from, to] = mac ? ["Ctrl", "Cmd"] : ["Cmd", "Ctrl"];
  const other = mac ? "Windows" : "Mac";
  const logical =
    device.scale !== 100 && device.rect
      ? t("side.logical", { w: Math.round(device.rect.w), h: Math.round(device.rect.h) })
      : "";
  const state = device.online ? t("tag.online") : t("tag.offline");
  const near = neighbours(device.fingerprint, touching).flatMap((n) => {
    const d = devices.find((x) => x.fingerprint === n.id);
    return d ? [{ side: n.side, device: d }] : [];
  });
  const lonely = device.rect && device.online && near.length === 0 && devices.length > 1;

  /** Turn the swap on or off for input into this device */
  const toggleSwap = () => {
    const next = !swap;
    setSwap(next);
    api.setSwap(next).catch((e) => {
      setSwap(!next);
      onToast(formatError(t, e));
    });
  };

  /** Remove the member (second click) */
  const kick = () =>
    press(device.fingerprint, () => {
      api
        .kick(device.fingerprint)
        .then(() => onToast(t("toast.kickedMember", { name: device.name })))
        .catch((e) => onToast(formatError(t, e)));
    });

  return (
    <>
      <div className="side-head">
        <div className={`av${device.online ? "" : " off"}`}>
          <PlatformIcon platform={device.platform} />
        </div>
        <div>
          <b>
            {device.name}
            {device.local && <span className="tag">{t("tag.local")}</span>}
          </b>
          <small>
            {t(mac ? "platform.macos" : "platform.windows")}
            {device.number !== null && ` · ${t("side.number", { n: device.number })}`} · {state}
          </small>
        </div>
      </div>
      <dl className="kv">
        <dt>{t("side.displays")}</dt>
        <dd>{t("side.displaysValue", { n: device.displays, res: device.resolution })}</dd>
        <dt>{t("side.scale")}</dt>
        <dd>
          {device.scale}% <em>{logical}</em>
        </dd>
        <dt>{t("side.neighbours")}</dt>
        <dd>
          {near.length === 0 ? (
            <em>{t("side.none")}</em>
          ) : (
            near.map((n) => (
              <div key={`${n.side}${n.device.fingerprint}`}>
                {t(`side.${n.side}`)}：{n.device.name}
                {!n.device.online && <em> ({t("tag.offline")})</em>}
              </div>
            ))
          )}
        </dd>
      </dl>
      {lonely && (
        <div className="warnbox">
          <WarnIcon />
          <span>
            {t("side.lonely", { alt: altKey(snapshot.device.platform), n: device.number ?? "" })}
          </span>
        </div>
      )}
      <div className="hr" />
      <div className="opt">
        <div>
          <b>{t("side.swap")}</b>
          <small>
            {device.local
              ? t("side.swapLocal", { other, from, to })
              : t("side.swapRemote", { state: t(device.swap ? "side.on" : "side.off") })}
          </small>
        </div>
        <button
          className={`tg${swap ? " on" : ""}`}
          disabled={!device.local}
          onClick={toggleSwap}
          aria-label={t("side.swap")}
        />
      </div>
      {!device.local && (
        <button
          className={`btn wide ${armed === device.fingerprint ? "armed" : "danger"}`}
          onClick={kick}
        >
          {t(armed === device.fingerprint ? "side.kickConfirm" : "side.kick")}
        </button>
      )}
      <div className="tip">
        <b>{t("side.tipTitle")}</b>
        {t("side.tip")}
      </div>
    </>
  );
}

/** Outside a group: how to join, and the devices nearby */
function JoinPanel({ nearby, onJoin }: { nearby: NearbyDto[]; onJoin: (target: NearbyDto) => void }) {
  const { t } = useI18n();
  return (
    <>
      <div className="side-head">
        <div className="av">
          <Logo />
        </div>
        <div>
          <b>{t("joinPanel.title")}</b>
          <small>{t("joinPanel.subtitle")}</small>
        </div>
      </div>
      <p>{t("joinPanel.body")}</p>
      <div className="nearby">
        <div className="group-h">{t("joinPanel.nearby")}</div>
        {nearby.length === 0 && <p className="muted">{t("devices.nearbyNone")}</p>}
        {nearby.map((n) => (
          <div className="row" key={n.fingerprint}>
            <div className="av sm">
              <PlatformIcon platform={n.platform} />
            </div>
            <div className="who">
              <b>{n.name}</b>
              <small>{t(n.group ? "devices.otherGroup" : "devices.noGroup")}</small>
            </div>
            <button className="btn sm primary" onClick={() => onJoin(n)}>
              {t("devices.join")}
            </button>
          </div>
        ))}
      </div>
      <div className="tip">{t("joinPanel.tip")}</div>
    </>
  );
}
