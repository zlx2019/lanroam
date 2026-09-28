// Settings → Switching: the hotkeys, how the pointer crosses edges by
// default and edge by edge, and the key combinations kept on this machine. Combinations are
// recorded by the engine from the physical keys (this device's keyboard,
// or the controlling device's), so even ones the OS or a hotkey would take
// are seen; changes save at once.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import { altKey, formatError, useI18n } from "../i18n";
import { DEFAULT_HOTKEYS, chordLabels, commanding, modLabels, modsOf, sameChord, validHotkey } from "../keys";
import type { Chord, HoldKey, InputSettings, Snapshot, SwitchMode } from "../types";
import { Keycaps, Row, Seg, Stepper } from "./controls";
import { CORNER, EdgeList } from "./EdgeList";

/** What a recording is for */
type Target = "pause" | "lock" | "jump" | "step" | "keep";

/** Dwell limits in the interface (ms) */
const DWELL = { min: 100, max: 1000, step: 50 };

/** The switching settings */
export function SwitchingSettings({
  snapshot,
  onToast,
}: {
  snapshot: Snapshot;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [settings, setSettings] = useState<InputSettings | null>(null);
  const [names, setNames] = useState<Record<string, string>>({});
  const [recording, setRecording] = useState<Target | null>(null);
  const platform = snapshot.device.platform;

  useEffect(() => {
    api.getInputSettings().then(setSettings).catch(console.error);
    api.keyNames().then(setNames).catch(console.error);
  }, []);

  /** Save and use new settings */
  const save = (next: InputSettings) =>
    api
      .saveInputSettings(next)
      .then(() => setSettings(next))
      .catch((e) => onToast(formatError(t, e)));

  // While recording: the combination arrives as an event; leaving the
  // window or the page stops it, or it would catch keys meant elsewhere
  useEffect(() => {
    if (!recording || !settings) return;
    const unlisten = listen<Chord | null>(EVENTS.RECORDED, (e) => {
      setRecording(null);
      if (e.payload) apply(recording, e.payload);
    });
    const stop = () => setRecording(null);
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("blur", stop);
      unlisten.then((u) => u()).catch(console.error);
      api.recordKeys(false).catch(console.error);
    };
    // `apply` reads the settings this effect depends on
  }, [recording, settings]);

  /** Start recording for `target`, or stop when it is already */
  const record = (target: Target) => {
    if (recording === target) {
      setRecording(null);
      return;
    }
    setRecording(target);
    api.recordKeys(true).catch((e) => {
      setRecording(null);
      onToast(formatError(t, e));
    });
  };

  /** Put a recorded combination where it was recorded for */
  const apply = (target: Target, chord: Chord) => {
    if (!settings) return;
    const words = { alt: altKey(platform), meta: platform === "macos" ? "Cmd" : "Win" };
    if (target === "keep") {
      if (settings.keepLocal.some((c) => sameChord(c, chord))) return onToast(t("switch.duplicate"));
      save({ ...settings, keepLocal: [...settings.keepLocal, chord] });
      return;
    }
    if (target === "jump" || target === "step") {
      if (!commanding(chord)) return onToast(t("switch.invalid", words));
      save({ ...settings, hotkeys: { ...settings.hotkeys, [target]: modsOf(chord) } });
      return;
    }
    if (!validHotkey(chord)) return onToast(t("switch.invalid", words));
    const other = target === "pause" ? settings.hotkeys.lock : settings.hotkeys.pause;
    if (sameChord(other, chord)) return onToast(t("switch.taken"));
    save({ ...settings, hotkeys: { ...settings.hotkeys, [target]: chord } });
  };

  if (!settings) return null;
  const { hotkeys, switching } = settings;
  const change = (patch: Partial<typeof switching>) => save({ ...settings, switching: { ...switching, ...patch } });

  /** Keycaps to click, or the recording prompt */
  const keys = (target: Target, caps: string[]) =>
    recording === target ? (
      <span className="keys rec" onClick={() => record(target)}>
        {t("switch.recording")}
      </span>
    ) : (
      <span className="keys" onClick={() => record(target)} title={t("switch.change")}>
        <Keycaps keys={caps} />
      </span>
    );

  return (
    <>
      <div className="group-h">
        {t("switch.hotkeys")} <span className="muted">· {t("switch.hotkeysHint")}</span>
      </div>
      <div className="group">
        <Row title={t("switch.pause")}>{keys("pause", chordLabels(hotkeys.pause, platform, names))}</Row>
        <Row title={t("switch.lock")} hint={t("switch.lockHint")}>
          {keys("lock", chordLabels(hotkeys.lock, platform, names))}
        </Row>
        <Row title={t("switch.jump")}>{keys("jump", [...modLabels(hotkeys.jump, platform), "1–9"])}</Row>
        <Row title={t("switch.step")}>{keys("step", [...modLabels(hotkeys.step, platform), t("switch.arrows")])}</Row>
        <div className="srow end">
          <button className="btn ghost" onClick={() => save({ ...settings, hotkeys: DEFAULT_HOTKEYS })}>
            {t("switch.reset")}
          </button>
        </div>
      </div>

      <div className="group-h">{t("switch.edges")}</div>
      <div className="group">
        <Row title={t("switch.mode")}>
          <Seg<SwitchMode>
            options={[
              ["direct", t("switch.direct")],
              ["modifier", t("switch.modifier")],
              ["dwell", t("switch.dwell")],
            ]}
            value={switching.mode}
            onChange={(mode) => change({ mode })}
          />
        </Row>
        {switching.mode === "modifier" && (
          <Row title={t("switch.hold")}>
            <Seg<HoldKey>
              options={[
                ["shift", "Shift"],
                ["ctrl", "Ctrl"],
                ["alt", altKey(platform)],
              ]}
              value={switching.hold}
              onChange={(hold) => change({ hold })}
            />
          </Row>
        )}
        {switching.mode === "dwell" && (
          <Row title={t("switch.dwellTime")}>
            <Stepper {...DWELL} unit="ms" value={switching.dwellMs} onChange={(dwellMs) => change({ dwellMs })} />
          </Row>
        )}
        <Row title={t("switch.corner")} hint={t("switch.cornerHint")}>
          <Stepper {...CORNER} unit="px" value={switching.cornerPx} onChange={(cornerPx) => change({ cornerPx })} />
        </Row>
      </div>

      <div className="group-h">{t("edges.title")}</div>
      <EdgeList group={snapshot.group} defaults={switching} onToast={onToast} />

      <div className="group-h">{t("switch.keepLocal")}</div>
      <div className="group">
        <div className="srow">
          <div className="t">
            <span className="chips">
              {settings.keepLocal.map((chord, i) => (
                <span key={i} className="kchip">
                  {chordLabels(chord, platform, names).join(" ")}
                  <button
                    aria-label={t("switch.remove")}
                    onClick={() => save({ ...settings, keepLocal: settings.keepLocal.filter((_, j) => j !== i) })}
                  >
                    ×
                  </button>
                </span>
              ))}
            </span>
            <small>{t("switch.keepLocalHint")}</small>
          </div>
          {recording === "keep" ? (
            <span className="keys rec" onClick={() => record("keep")}>
              {t("switch.recording")}
            </span>
          ) : (
            <button className="btn" onClick={() => record("keep")}>
              {t("switch.add")}
            </button>
          )}
        </div>
      </div>
    </>
  );
}
