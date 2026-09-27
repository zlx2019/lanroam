// The settings of one edge between two devices, opened from its junction
// on the layout canvas: whether the pointer crosses it, its corner guard
// and how it crosses. They belong to the group: every member follows them,
// both ways.

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { Touch } from "../geometry";
import type { EdgeSettings, Switching, SwitchMode } from "../types";
import { Seg, Stepper, Toggle } from "./controls";
import { CrossIcon } from "./icons";
import { CORNER } from "./SwitchingSettings";

/** Width of the popover (px) */
const WIDTH = 300;

/** The edge's settings next to its junction at (`x`, `y`) */
export function EdgePopover({
  edge,
  names,
  settings,
  x,
  y,
  width,
  onClose,
  onToast,
}: {
  edge: Touch;
  names: [string, string];
  settings: EdgeSettings;
  /** The junction, in canvas pixels */
  x: number;
  y: number;
  /** The canvas width, to open towards the room there is */
  width: number;
  onClose: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const ref = useRef<HTMLDivElement>(null);
  const [defaults, setDefaults] = useState<Switching | null>(null);

  useEffect(() => {
    api
      .getInputSettings()
      .then((s) => setDefaults(s.switching))
      .catch(console.error);
  }, []);

  // A press anywhere else, or Esc, closes it; the junctions toggle it
  // themselves
  useEffect(() => {
    const away = (e: PointerEvent) => {
      const el = e.target instanceof Element ? e.target : null;
      if (!ref.current?.contains(el) && !el?.closest(".jn")) onClose();
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [onClose]);

  /** Save a change for the whole group */
  const change = (patch: Partial<EdgeSettings>) =>
    api.setEdge(edge.a.id, edge.b.id, { ...settings, ...patch }).catch((e) => onToast(formatError(t, e)));

  const left = x + 18 + WIDTH > width ? x - 18 - WIDTH : x + 18;
  const corner = settings.cornerPx ?? defaults?.cornerPx ?? 0;
  const sides = t(edge.dir === "h" ? "edge.sidesH" : "edge.sidesV", { a: names[0], b: names[1] });
  return (
    <div ref={ref} className="pop" style={{ left, top: Math.max(10, y - 60), width: WIDTH }}>
      <div className="pop-h">
        <b>
          {names[0]} ⇄ {names[1]}
        </b>
        <button className="x" onClick={onClose} aria-label={t("edge.close")}>
          <CrossIcon />
        </button>
      </div>
      <small>{sides}</small>
      <MapDiagram edge={edge} names={names} />
      <div className="prow">
        <span>{t("edge.crossable")}</span>
        <Toggle on={settings.crossable} label={t("edge.crossable")} onChange={(crossable) => change({ crossable })} />
      </div>
      <div className="prow">
        <span>
          {t("edge.corner")}
          {settings.cornerPx === null && <em className="muted"> · {t("edge.followsDefault")}</em>}
        </span>
        <Stepper {...CORNER} unit="px" value={corner} onChange={(cornerPx) => change({ cornerPx })} />
      </div>
      <div className="prow col">
        <span>{t("edge.mode")}</span>
        <Seg<SwitchMode | "default">
          options={[
            ["default", t("edge.default")],
            ["direct", t("edge.direct")],
            ["modifier", t("edge.modifier")],
            ["dwell", t("edge.dwell")],
          ]}
          value={settings.mode ?? "default"}
          onChange={(mode) => change({ mode: mode === "default" ? null : mode })}
        />
      </div>
      {(settings.cornerPx !== null || settings.mode !== null || !settings.crossable) && (
        <button
          className="btn sm ghost"
          onClick={() => change({ crossable: true, cornerPx: null, mode: null })}
        >
          {t("edge.reset")}
        </button>
      )}
    </div>
  );
}

/** Both sides drawn to scale, joined where the pointer lands: the whole
 * of each side crosses, proportionally */
function MapDiagram({ edge, names }: { edge: Touch; names: [string, string] }) {
  const la = edge.dir === "h" ? edge.a.h : edge.a.w;
  const lb = edge.dir === "h" ? edge.b.h : edge.b.w;
  const k = 58 / Math.max(la, lb);
  const [ha, hb] = [la * k, lb * k];
  const height = Math.max(ha, hb) + 20;
  return (
    <svg className="map" width={WIDTH - 28} height={height} viewBox={`0 0 ${WIDTH - 28} ${height}`}>
      <rect x="100" y="10" width="6" height={ha} rx="2" fill="var(--accent)" />
      <rect x="166" y="10" width="6" height={hb} rx="2" fill="var(--accent)" />
      {[0, 0.25, 0.5, 0.75, 1].map((f) => (
        <line
          key={f}
          x1="106"
          y1={10 + f * ha}
          x2="166"
          y2={10 + f * hb}
          stroke="var(--accent)"
          strokeOpacity={f === 0.5 ? 0.9 : 0.4}
          strokeWidth="1"
        />
      ))}
      <text x="94" y={14 + ha / 2} textAnchor="end">
        {names[0].slice(0, 14)}
      </text>
      <text x="178" y={14 + hb / 2}>
        {names[1].slice(0, 15)}
      </text>
    </svg>
  );
}
