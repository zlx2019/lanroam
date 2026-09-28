// Settings → Keyboard and mouse. The Cmd ↔ Ctrl swap and the pointer
// speed belong to this device's entry in the group (the controlling device
// follows them), so they need a group; where media keys go and how
// scrolling is replayed here are this device's own input settings.

import { useEffect, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { InputSettings, MediaKeys, Scrolling, Snapshot } from "../types";
import { Row, Seg, Slider, Toggle } from "./controls";

/** Pointer speed limits in the interface (percent), as lanroam-input's */
const POINTER = { min: 50, max: 200, step: 10 };

/** Scrolling speed limits in the interface (percent) */
const SCROLL = { min: 50, max: 300, step: 10 };

/** The keyboard and mouse settings */
export function KeyboardMouseSettings({
  snapshot,
  onToast,
}: {
  snapshot: Snapshot;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [settings, setSettings] = useState<InputSettings | null>(null);
  const local = snapshot.group?.devices.find((d) => d.local) ?? null;
  const [swap, setSwap] = useState(local?.swap ?? true);
  useEffect(() => setSwap(local?.swap ?? true), [local?.swap]);

  useEffect(() => {
    api.getInputSettings().then(setSettings).catch(console.error);
  }, []);

  /** Save and use new input settings */
  const save = (next: InputSettings) =>
    api
      .saveInputSettings(next)
      .then(() => setSettings(next))
      .catch((e) => onToast(formatError(t, e)));

  /** Turn the swap on or off, for the group */
  const toggleSwap = (next: boolean) => {
    setSwap(next);
    api.setSwap(next).catch((e) => {
      setSwap(!next);
      onToast(formatError(t, e));
    });
  };

  /** Set the pointer speed, for the group; the snapshot brings it back */
  const setPointerSpeed = (speed: number) => api.setPointerSpeed(speed).catch((e) => onToast(formatError(t, e)));

  if (!settings) return null;
  const mac = snapshot.device.platform === "macos";
  const [from, to] = mac ? ["Ctrl", "Cmd"] : ["Cmd", "Ctrl"];
  const other = mac ? "Windows" : "Mac";
  const noGroup = local ? undefined : t("input.noGroup");
  const scroll = (patch: Partial<Scrolling>) => save({ ...settings, scrolling: { ...settings.scrolling, ...patch } });

  return (
    <>
      <div className="group-h">{t("input.keyboard")}</div>
      <div className="group">
        <Row title={t("side.swap")} hint={noGroup ?? t("side.swapLocal", { other, from, to })}>
          <button
            className={`tg${swap ? " on" : ""}`}
            disabled={!local}
            onClick={() => toggleSwap(!swap)}
            aria-label={t("side.swap")}
          />
        </Row>
        <Row title={t("input.media")} hint={t("input.mediaHint")}>
          <Seg<MediaKeys>
            options={[
              ["remote", t("input.mediaRemote")],
              ["local", t("input.mediaLocal")],
            ]}
            value={settings.mediaKeys}
            onChange={(mediaKeys) => save({ ...settings, mediaKeys })}
          />
        </Row>
      </div>

      <div className="group-h">
        {t("input.mouse")} <span className="muted">· {t("input.mouseHint")}</span>
      </div>
      <div className="group">
        <Row title={t("input.pointer")} hint={noGroup ?? t("input.pointerHint")}>
          <Slider
            {...POINTER}
            value={local?.pointerSpeed ?? 100}
            label={t("input.pointer")}
            disabled={!local}
            onChange={setPointerSpeed}
          />
        </Row>
        <Row title={t("input.scroll")}>
          <Slider
            {...SCROLL}
            value={settings.scrolling.speed}
            label={t("input.scroll")}
            onChange={(speed) => scroll({ speed })}
          />
        </Row>
        <Row title={t("input.reverse")} hint={t("input.reverseHint")}>
          <Toggle
            on={settings.scrolling.reverse}
            label={t("input.reverse")}
            onChange={(reverse) => scroll({ reverse })}
          />
        </Row>
      </div>
    </>
  );
}
