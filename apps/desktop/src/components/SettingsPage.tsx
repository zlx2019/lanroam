// The settings page. Changes apply at once: choices on click, the name on
// Enter or when the field loses focus.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { SettingsDto, Snapshot } from "../types";

/** Sections of the settings page */
type Section = "general" | "about";

/** The settings page */
export function SettingsPage({
  snapshot,
  settings,
  onSettings,
  onToast,
}: {
  snapshot: Snapshot;
  settings: SettingsDto;
  onSettings: (next: SettingsDto) => Promise<void>;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [section, setSection] = useState<Section>("general");
  return (
    <div className="settings">
      <nav className="snav">
        {(["general", "about"] as const).map((id) => (
          <button key={id} className={section === id ? "on" : ""} onClick={() => setSection(id)}>
            {t(`settings.${id}`)}
          </button>
        ))}
      </nav>
      <div className="pane">
        <div className="pane-inner" style={{ maxWidth: 640 }}>
          {section === "general" ? (
            <General snapshot={snapshot} settings={settings} onSettings={onSettings} onToast={onToast} />
          ) : (
            <About snapshot={snapshot} />
          )}
        </div>
      </div>
    </div>
  );
}

/** A segmented choice */
function Seg<T extends string>({
  options,
  value,
  onChange,
}: {
  options: [T, string][];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <span className="seg">
      {options.map(([id, label]) => (
        <button key={id} className={value === id ? "on" : ""} onClick={() => onChange(id)}>
          {label}
        </button>
      ))}
    </span>
  );
}

/** One row of a settings group */
function Row({ title, hint, children }: { title: string; hint?: string; children: ReactNode }) {
  return (
    <div className="srow">
      <div className="t">
        <b>{title}</b>
        {hint && <small>{hint}</small>}
      </div>
      {children}
    </div>
  );
}

/** Name, start at login, language, theme */
function General({
  snapshot,
  settings,
  onSettings,
  onToast,
}: {
  snapshot: Snapshot;
  settings: SettingsDto;
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
        <Row title={t("settings.name")} hint={t("settings.nameHint")}>
          <input
            className="input"
            style={{ width: 220 }}
            value={name}
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
        <Row title={t("settings.autostart")} hint={t("settings.autostartHint")}>
          <button
            className={`tg${settings.autostart ? " on" : ""}`}
            onClick={() => change({ autostart: !settings.autostart })}
            aria-label={t("settings.autostart")}
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

/** Version, identity, source */
function About({ snapshot }: { snapshot: Snapshot }) {
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
    <>
      <div className="group">
        <Row title={t("about.version")}>
          <span className="muted">{snapshot.device.version}</span>
        </Row>
        <Row title={t("about.fingerprint")} hint={t("about.fingerprintHint")}>
          <span className="chips">
            <span className="fp">{fp.slice(0, 16).replace(/(.{4})/g, "$1 ")}…</span>
            <button className="btn sm" onClick={copy}>
              {t(copied ? "about.copied" : "about.copy")}
            </button>
          </span>
        </Row>
        {snapshot.group && (
          <Row title={t("about.group")}>
            <span className="fp">{snapshot.group.id}</span>
          </Row>
        )}
      </div>
      <div className="group">
        <Row title={t("about.source")} hint={t("about.license")}>
          <span className="muted">github.com/zlx2019/lanroam</span>
        </Row>
      </div>
    </>
  );
}
