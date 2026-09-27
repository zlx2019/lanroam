// The layout page: the group's screens on the canvas and the selected
// device beside it; outside a group, the devices to join instead.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api } from "../api";
import { useArmed } from "../hooks/useLanroam";
import { formatError, useI18n } from "../i18n";
import type { DeviceDto, NearbyDto, Snapshot } from "../types";
import { CursorIcon, InIcon, InfoIcon, LockIcon, Logo, PlatformIcon, WarnIcon } from "./icons";

/** Room around the devices when fitting them in the canvas (px) */
const FIT_PADDING = 70;

/** Largest scale: a lone device must not fill the whole canvas */
const MAX_SCALE = 0.2;

/** Canvas to screen: scale, then offset */
interface View {
  k: number;
  ox: number;
  oy: number;
}

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
  /** Just joined: say how crossing works */
  joined: boolean;
  onJoin: (target: NearbyDto) => void;
  onJoinedSeen: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const devices = snapshot.group?.devices ?? [];
  const [selected, setSelected] = useState(snapshot.device.fingerprint);
  const current = devices.find((d) => d.fingerprint === selected) ?? devices.find((d) => d.local);
  const alt = altKey(snapshot.device.platform);
  const inputProblem = snapshot.input.capture
    ? t("input.captureOff", { reason: snapshot.input.capture })
    : snapshot.input.injection
      ? t("input.injectionOff", { reason: snapshot.input.injection })
      : null;
  const unplaced = devices.filter((d) => !d.rect).length;

  return (
    <div className="layout">
      <div className="canvas">
        {snapshot.group ? (
          <Canvas snapshot={snapshot} selected={current?.fingerprint} onSelect={setSelected} />
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
        {snapshot.group ? (
          <div className="legend">
            <span>
              <i />
              {t("layout.legend.edge")}
            </span>
            <span>{t("layout.legend.numbers", { alt })}</span>
            {unplaced > 0 && <span>{t("layout.unplaced", { n: unplaced })}</span>}
          </div>
        ) : (
          <div className="hint">{t("layout.ungrouped")}</div>
        )}
      </div>
      <aside className="side">
        {snapshot.group && current ? (
          <SidePanel snapshot={snapshot} device={current} onToast={onToast} />
        ) : (
          <JoinPanel nearby={nearby} onJoin={onJoin} />
        )}
      </aside>
    </div>
  );
}

/** The group's devices on the canvas, fitted to its size */
function Canvas({
  snapshot,
  selected,
  onSelect,
}: {
  snapshot: Snapshot;
  selected?: string;
  onSelect: (fingerprint: string) => void;
}) {
  const { t } = useI18n();
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const placed = (snapshot.group?.devices ?? []).filter((d) => d.rect);
  const view = fit(placed, size.w, size.h);
  const { control } = snapshot;
  return (
    <div ref={ref} style={{ position: "absolute", inset: 0 }}>
      {view &&
        placed.map((d) => {
          const r = d.rect!;
          const w = r.w * view.k;
          const h = r.h * view.k;
          const target = control.peer === d.name && !d.local;
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
          return (
            <div
              key={d.fingerprint}
              className={`dev${d.online ? "" : " offline"}${d.fingerprint === selected ? " sel" : ""}`}
              style={{ left: view.ox + r.x * view.k, top: view.oy + r.y * view.k, width: w, height: h }}
              onClick={() => onSelect(d.fingerprint)}
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
    </div>
  );
}

/** Scale and offset that fit `devices` into a `w`×`h` canvas */
function fit(devices: DeviceDto[], w: number, h: number): View | null {
  const rects = devices.flatMap((d) => (d.rect ? [d.rect] : []));
  if (!rects.length || !w || !h) return null;
  const minX = Math.min(...rects.map((r) => r.x));
  const minY = Math.min(...rects.map((r) => r.y));
  const maxX = Math.max(...rects.map((r) => r.x + r.w));
  const maxY = Math.max(...rects.map((r) => r.y + r.h));
  const k = Math.min(
    (w - 2 * FIT_PADDING) / (maxX - minX),
    (h - 2 * FIT_PADDING) / (maxY - minY),
    MAX_SCALE,
  );
  return {
    k,
    ox: (w - (maxX - minX) * k) / 2 - minX * k,
    oy: (h - (maxY - minY) * k) / 2 - minY * k,
  };
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

/** The selected member: what it is, and what can be done with it */
function SidePanel({
  snapshot,
  device,
  onToast,
}: {
  snapshot: Snapshot;
  device: DeviceDto;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [armed, press] = useArmed();
  const [swap, setSwap] = useState(device.swap);
  useEffect(() => setSwap(device.swap), [device.swap]);
  const mac = device.platform === "macos";
  const [from, to] = mac ? ["Ctrl", "Cmd"] : ["Cmd", "Ctrl"];
  const other = mac ? "Windows" : "Mac";
  const logical =
    device.scale !== 100 && device.rect
      ? t("side.logical", { w: Math.round(device.rect.w), h: Math.round(device.rect.h) })
      : "";
  const state = device.online ? t("tag.online") : t("tag.offline");

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
      </dl>
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
      <div className="tip">{t("side.tip", { alt: altKey(snapshot.device.platform) })}</div>
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
