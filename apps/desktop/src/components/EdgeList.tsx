// Settings → Control → Neighbours: one row for every pair of devices whose
// screens touch, saying in words whether the pointer crosses there and how,
// with a switch; unfolded, how it crosses and its corner guard, over the
// defaults. They belong to the group: every member follows them, both ways.

import { useState } from "react";
import { api } from "../api";
import { pairs, seams, type Seam } from "../geometry";
import { formatError, useI18n } from "../i18n";
import type { EdgeSettings, GroupDto, Switching, SwitchMode } from "../types";
import { Row, Seg, Stepper, Toggle } from "./controls";
import { ChevronIcon } from "./icons";

/** Corner guard limits in the interface (px) */
export const CORNER = { min: 0, max: 40, step: 2 };

/** Every edge of `group`, over the switching defaults */
export function EdgeList({
  group,
  defaults,
  onToast,
}: {
  group: GroupDto | null;
  defaults: Switching;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState<string | null>(null);
  const placed = (group?.devices ?? []).map((d) => ({ id: d.fingerprint, screens: d.screens }));
  const edges = pairs(seams(placed));
  if (!group || edges.length === 0) {
    return (
      <div className="group">
        <Row title={t("edges.none")} hint={t("edges.noneHint")}>
          {null}
        </Row>
      </div>
    );
  }
  return (
    <div className="group">
      {edges.map((seam) => {
        const key = `${seam.a}|${seam.b}`;
        return (
          <Edge
            key={key}
            group={group}
            seam={seam}
            defaults={defaults}
            open={open === key}
            onOpen={() => setOpen(open === key ? null : key)}
            onToast={onToast}
          />
        );
      })}
    </div>
  );
}

/** One edge: its two devices, a summary, the switch; unfolded, the rest */
function Edge({
  group,
  seam,
  defaults,
  open,
  onOpen,
  onToast,
}: {
  group: GroupDto;
  seam: Seam;
  defaults: Switching;
  open: boolean;
  onOpen: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const settings = edgeSettings(group, seam.a, seam.b);
  const name = (id: string) => group.devices.find((d) => d.fingerprint === id)?.name ?? "";

  /** Save a change for the whole group */
  const change = (patch: Partial<EdgeSettings>) =>
    api.setEdge(seam.a, seam.b, { ...settings, ...patch }).catch((e) => onToast(formatError(t, e)));

  const modes: Record<SwitchMode, string> = {
    direct: t("switch.direct"),
    modifier: t("switch.modifier"),
    dwell: t("switch.dwell"),
  };
  // What happens there, in words: its own settings, or the defaults
  const own = [
    settings.mode ? modes[settings.mode] : "",
    settings.cornerPx !== null ? t("edges.corner", { n: settings.cornerPx }) : "",
  ].filter(Boolean);
  const summary = settings.crossable
    ? [t("edges.open"), ...(own.length ? own : [t("edges.followsDefault")])].join(" · ")
    : t("edges.closed");
  const custom = settings.cornerPx !== null || settings.mode !== null || !settings.crossable;
  return (
    <>
      <div className="srow edge-row">
        <div className="t">
          <b>
            <span className="pair">
              {name(seam.a)} <i>{seam.line === "v" ? "⇄" : "⇅"}</i> {name(seam.b)}
            </span>
          </b>
          <small>{summary}</small>
        </div>
        <Toggle on={settings.crossable} label={t("edges.crossable")} onChange={(crossable) => change({ crossable })} />
        <button className={`chev${open ? " open" : ""}`} onClick={onOpen} aria-label={t("edges.more")}>
          <ChevronIcon />
        </button>
      </div>
      {open && (
        <div className="edge-more">
          <span>{t("switch.mode")}</span>
          <Seg<SwitchMode | "default">
            options={[
              ["default", t("edges.default")],
              ["direct", modes.direct],
              ["modifier", modes.modifier],
              ["dwell", modes.dwell],
            ]}
            value={settings.mode ?? "default"}
            onChange={(mode) => change({ mode: mode === "default" ? null : mode })}
          />
          <span>{t("switch.corner")}</span>
          <Stepper
            {...CORNER}
            unit="px"
            value={settings.cornerPx ?? defaults.cornerPx}
            onChange={(cornerPx) => change({ cornerPx })}
          />
          {custom && (
            <>
              <span />
              <button className="link" onClick={() => change({ crossable: true, cornerPx: null, mode: null })}>
                {t("edges.reset")}
              </button>
            </>
          )}
        </div>
      )}
    </>
  );
}

/** The settings of the edge between `a` and `b` in `group` (defaults when
 * it has none of its own) */
function edgeSettings(group: GroupDto, a: string, b: string): EdgeSettings {
  const own = group.edges.find((e) => (e.a === a && e.b === b) || (e.a === b && e.b === a));
  return own
    ? { crossable: own.crossable, cornerPx: own.cornerPx, mode: own.mode }
    : { crossable: true, cornerPx: null, mode: null };
}
