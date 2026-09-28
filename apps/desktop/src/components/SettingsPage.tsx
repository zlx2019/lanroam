// The settings page. Changes apply at once: choices on click, the name on
// Enter or when the field loses focus.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { InputSettings, SettingsDto, Snapshot } from "../types";
import { applyOpacity } from "../theme";
import { Row, Seg, Slider, Toggle } from "./controls";
import { ClipboardSettings } from "./ClipboardSettings";
import { ClipboardIcon, EyeIcon, GearIcon, InfoIcon, SwitchIcon } from "./icons";
import { KeyboardMouseSettings } from "./KeyboardMouseSettings";
import { SwitchingSettings } from "./SwitchingSettings";

/** Sections of the settings page */
export type Section = "general" | "control" | "clipboard" | "look" | "about";

/** Sections in order, with their icons */
const SECTIONS: [Section, ReactNode][] = [
  ["general", <GearIcon key="general" />],
  ["control", <SwitchIcon key="control" />],
  ["clipboard", <ClipboardIcon key="clipboard" />],
  ["look", <EyeIcon key="look" />],
  ["about", <InfoIcon key="about" />],
];

/** The settings page; `renaming` puts the cursor in the name field */
export function SettingsPage({
  snapshot,
  settings,
  section,
  renaming,
  onSection,
  onSettings,
  onToast,
}: {
  snapshot: Snapshot;
  settings: SettingsDto;
  section: Section;
  renaming: boolean;
  onSection: (section: Section) => void;
  onSettings: (next: SettingsDto) => Promise<void>;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  return (
    <div className="pane settings">
      <nav className="rail">
        {SECTIONS.map(([id, icon]) => (
          <button key={id} className={section === id ? "on" : ""} onClick={() => onSection(id)}>
            {icon}
            {t(`settings.${id}`)}
          </button>
        ))}
      </nav>
      <div>
        {section === "general" ? (
          <General
            snapshot={snapshot}
            settings={settings}
            renaming={renaming}
            onSettings={onSettings}
            onToast={onToast}
          />
        ) : section === "control" ? (
          <Control snapshot={snapshot} onToast={onToast} />
        ) : section === "clipboard" ? (
          <ClipboardSettings snapshot={snapshot} onToast={onToast} />
        ) : section === "look" ? (
          <Look settings={settings} onSettings={onSettings} onToast={onToast} />
        ) : (
          <About snapshot={snapshot} onToast={onToast} />
        )}
      </div>
    </div>
  );
}

/** Name, start at login, closing the window, language, theme */
function General({
  snapshot,
  settings,
  renaming,
  onSettings,
  onToast,
}: {
  snapshot: Snapshot;
  settings: SettingsDto;
  renaming: boolean;
  onSettings: (next: SettingsDto) => Promise<void>;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(snapshot.device.name);
  // Esc reverts, and the blur it causes must not save
  const cancelled = useRef(false);
  useEffect(() => setName(snapshot.device.name), [snapshot.device.name]);

  /** Save the name if it changed */
  const commitName = () => {
    if (cancelled.current) {
      cancelled.current = false;
      return;
    }
    if (name.trim() === snapshot.device.name) return;
    api
      .rename(name)
      .then(() => onToast(t("toast.renamed")))
      .catch((e) => {
        setName(snapshot.device.name);
        onToast(formatError(t, e));
      });
  };

  /** Apply one changed preference */
  const change = (patch: Partial<SettingsDto>) =>
    onSettings({ ...settings, ...patch }).catch((e) => onToast(formatError(t, e)));

  return (
    <>
      <div className="group">
        <Row title={t("settings.name")}>
          <input
            className="input"
            value={name}
            autoFocus={renaming}
            onFocus={(e) => renaming && e.currentTarget.select()}
            maxLength={40}
            onChange={(e) => setName(e.target.value)}
            onBlur={commitName}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                cancelled.current = true;
                setName(snapshot.device.name);
                e.currentTarget.blur();
              }
            }}
          />
        </Row>
        <Row title={t("settings.autostart")}>
          <Toggle
            on={settings.autostart}
            label={t("settings.autostart")}
            onChange={(autostart) => change({ autostart })}
          />
        </Row>
        <Row title={t("settings.closeWindow")}>
          <Seg
            options={[
              ["tray", t("settings.closeHide")],
              ["quit", t("settings.closeQuit")],
            ]}
            value={settings.closeWindow}
            onChange={(closeWindow) => change({ closeWindow })}
          />
        </Row>
      </div>
      <div className="group">
        <Row title={t("settings.language")}>
          <Seg
            options={[
              ["system", t("settings.system")],
              ["zh", "中文"],
              ["en", "English"],
            ]}
            value={settings.language}
            onChange={(language) => change({ language })}
          />
        </Row>
        <Row title={t("settings.theme")}>
          <Seg
            options={[
              ["system", t("settings.system")],
              ["light", t("settings.light")],
              ["dark", t("settings.dark")],
            ]}
            value={settings.theme}
            onChange={(theme) => change({ theme })}
          />
        </Row>
      </div>
    </>
  );
}

/** Switching, then the keyboard and the mouse: both change the one copy of
 * the input settings held here, which each change saves whole */
function Control({ snapshot, onToast }: { snapshot: Snapshot; onToast: (message: string) => void }) {
  const { t } = useI18n();
  const [settings, setSettings] = useState<InputSettings | null>(null);

  useEffect(() => {
    api.getInputSettings().then(setSettings).catch(console.error);
  }, []);

  /** Save and use new input settings */
  const save = (next: InputSettings) => {
    api
      .saveInputSettings(next)
      .then(() => setSettings(next))
      .catch((e) => onToast(formatError(t, e)));
  };

  if (!settings) return null;
  return (
    <>
      <SwitchingSettings snapshot={snapshot} settings={settings} save={save} onToast={onToast} />
      <KeyboardMouseSettings snapshot={snapshot} settings={settings} save={save} onToast={onToast} />
    </>
  );
}

/** Opacity limits in the interface (percent), as settings.rs */
const OPACITY = { min: 40, max: 100, step: 5 };

/** The windows' opacity and the on-screen indicators */
function Look({
  settings,
  onSettings,
  onToast,
}: {
  settings: SettingsDto;
  onSettings: (next: SettingsDto) => Promise<void>;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();

  /** Apply one changed preference */
  const change = (patch: Partial<SettingsDto>) =>
    onSettings({ ...settings, ...patch }).catch((e) => onToast(formatError(t, e)));

  return (
    <>
      <div className="group">
        <Row title={t("settings.opacity")} hint={t("settings.opacityHint")}>
          <Slider
            {...OPACITY}
            value={settings.opacity}
            label={t("settings.opacity")}
            format={(v) => `${v}%`}
            onInput={applyOpacity}
            onChange={(opacity) => change({ opacity })}
          />
        </Row>
      </div>
      <div className="group">
        <Row title={t("settings.edgeGlow")} hint={t("settings.edgeGlowHint")}>
          <Toggle on={settings.edgeGlow} label={t("settings.edgeGlow")} onChange={(edgeGlow) => change({ edgeGlow })} />
        </Row>
        <Row title={t("settings.hints")}>
          <Toggle on={settings.hints} label={t("settings.hints")} onChange={(hints) => change({ hints })} />
        </Row>
        <Row title={t("settings.dim")}>
          <Toggle on={settings.dim} label={t("settings.dim")} onChange={(dim) => change({ dim })} />
        </Row>
      </div>
    </>
  );
}

/** A fingerprint by both ends, as `e852 1c76 … 90ca c1f3` */
function shortFingerprint(fp: string): string {
  const group = (hex: string) => hex.replace(/(.{4})(?=.)/g, "$1 ");
  return `${group(fp.slice(0, 8))} … ${group(fp.slice(-8))}`;
}

/** Version, identity, logs */
function About({ snapshot, onToast }: { snapshot: Snapshot; onToast: (message: string) => void }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const fp = snapshot.device.fingerprint;

  /** Copy the fingerprint */
  const copy = () => {
    navigator.clipboard
      .writeText(fp)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      })
      .catch(console.error);
  };

  return (
    <div className="group">
      <Row title={t("about.version")}>
        <span className="muted">{snapshot.device.version}</span>
      </Row>
      <Row title={t("about.fingerprint")}>
        <span className="chips">
          <span className="fp" title={fp}>
            {shortFingerprint(fp)}
          </span>
          <button className="btn" onClick={copy}>
            {t(copied ? "about.copied" : "about.copy")}
          </button>
        </span>
      </Row>
      <Row title={t("about.logs")}>
        <button className="btn" onClick={() => api.openLogs().catch((e) => onToast(formatError(t, e)))}>
          {t("about.openLogs")}
        </button>
      </Row>
    </div>
  );
}
