// One device's screens as tiles, the same in the arrangement panel and on
// the device cards: glass tiles with the system's logo in the middle; the
// primary screen carries the device's number and name when there is room.

import type { PointerEvent } from "react";
import { bounds, primaryIndex, type View } from "../geometry";
import { useI18n } from "../i18n";
import type { DeviceDto, RectDto } from "../types";
import { OsLogo } from "./icons";

/** Pointer handlers of a draggable device */
export interface DragHandlers {
  onPointerDown: (e: PointerEvent<HTMLDivElement>) => void;
  onPointerMove: (e: PointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: PointerEvent<HTMLDivElement>) => void;
  onPointerCancel: (e: PointerEvent<HTMLDivElement>) => void;
}

/** A device's screens at `screens` (canvas units), drawn through `view`;
 * `labels` adds the number, name and sizes, `drag` makes it draggable */
export function Screens({
  device,
  screens,
  view,
  labels,
  className = "",
  drag,
}: {
  device: DeviceDto;
  screens: RectDto[];
  view: View;
  labels: boolean;
  className?: string;
  drag?: DragHandlers;
}) {
  const { t } = useI18n();
  if (!screens.length) return null;
  const box = bounds(screens);
  const primary = primaryIndex(screens, device.origin && shiftedOrigin(device, screens));
  const cls = [
    "dev",
    drag && "drag",
    device.local && "local",
    !device.online && "offline",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      className={cls}
      style={{
        left: view.ox + box.x * view.k,
        top: view.oy + box.y * view.k,
        width: box.w * view.k,
        height: box.h * view.k,
      }}
      {...drag}
    >
      {labels && screens.length > 1 && <div className="aura" />}
      {screens.map((s, i) => (
        <Tile
          key={i}
          device={device}
          screen={s}
          at={{ x: (s.x - box.x) * view.k, y: (s.y - box.y) * view.k }}
          k={view.k}
          primary={i === primary}
          labels={labels}
          sub={device.local ? t("tag.local") : device.online ? device.resolution : t("tag.offline")}
        />
      ))}
    </div>
  );
}

/** One screen: the logo, and on the primary one the number and name */
function Tile({
  device,
  screen,
  at,
  k,
  primary,
  labels,
  sub,
}: {
  device: DeviceDto;
  screen: RectDto;
  at: { x: number; y: number };
  k: number;
  primary: boolean;
  labels: boolean;
  /** What the primary screen says under the name */
  sub: string;
}) {
  const w = screen.w * k;
  const h = screen.h * k;
  const size = !labels ? " bare" : w < 60 ? " tiny" : w < 130 && primary ? " small" : "";
  // The logo scales with the screen, and keeps clear of the name below it
  const logo = Math.min(Math.max(Math.min(w, h) * (labels ? 0.28 : 0.36), 10), 46);
  const logoY = labels && primary ? h * 0.44 : h / 2;
  return (
    <div
      className={`tile${primary ? " primary" : ""}${size}`}
      style={{ left: at.x, top: at.y, width: w, height: h }}
    >
      <i className="logo" style={{ left: w / 2, top: logoY, width: logo, height: logo }}>
        <OsLogo platform={device.platform} />
      </i>
      {primary ? (
        <>
          {device.number !== null && <span className="num">{device.number}</span>}
          <div className="name">
            <b>{device.name}</b>
            <small>{sub}</small>
          </div>
        </>
      ) : (
        !size && (
          <div className="name">
            <small>{deviceSize(screen, device.scale)}</small>
          </div>
        )
      )}
    </div>
  );
}

/** Where the device's origin is when its screens are moved to `screens`:
 * a drag moves them all together */
function shiftedOrigin(device: DeviceDto, screens: RectDto[]) {
  if (!device.origin || !device.rect) return device.origin;
  const box = bounds(screens);
  return { x: device.origin.x + box.x - device.rect.x, y: device.origin.y + box.y - device.rect.y };
}

/** A screen's size in the device's own units, e.g. 1920×1080 */
function deviceSize(screen: RectDto, scale: number): string {
  return `${Math.round((screen.w * scale) / 100)}×${Math.round((screen.h * scale) / 100)}`;
}
