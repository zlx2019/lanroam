// macOS only: walk through the two input permissions, then start the
// capture and injection that were refused without them.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../api";
import { useI18n } from "../i18n";
import type { PermissionsDto } from "../types";
import { CheckIcon, HandIcon, KeyboardIcon } from "./icons";

/** How often the permissions are checked while this page is up */
const POLL_MS = 1000;

/** The permissions page; `onDone` when granted (or skipped) */
export function Onboarding({ onDone }: { onDone: () => void }) {
  const { t } = useI18n();
  const [perms, setPerms] = useState<PermissionsDto | null>(null);
  const [relaunch, setRelaunch] = useState(false);
  const started = useRef(false);
  const granted = !!perms && perms.accessibility && perms.inputMonitoring;

  useEffect(() => {
    let alive = true;
    const check = () =>
      api
        .getPermissions()
        .then((p) => alive && setPerms(p))
        .catch(console.error);
    check();
    const timer = setInterval(check, POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  // Both granted: start what was refused; some grants only reach a new
  // process, and then only a restart helps
  useEffect(() => {
    if (!granted || started.current) return;
    started.current = true;
    api
      .restartInput()
      .then((input) => setRelaunch(!!input.capture || !!input.injection))
      .catch(console.error);
  }, [granted]);

  /** One permission's row */
  const row = (
    id: "accessibility" | "inputMonitoring",
    icon: ReactNode,
    title: string,
    hint: string,
  ) => (
    <div className="perm">
      <div className="av">{icon}</div>
      <div className="t">
        <b>{title}</b>
        <small>{hint}</small>
      </div>
      {perms?.[id] ? (
        <span className="ok">
          <CheckIcon />
          {t("perms.granted")}
        </span>
      ) : (
        <button className="btn primary" onClick={() => api.openPermission(id).catch(console.error)}>
          {t("perms.open")}
        </button>
      )}
    </div>
  );

  return (
    <div className="onb">
      <div className="onb-card">
        <div className="steps">
          <i className="on" />
          <i className={granted ? "on" : ""} />
          <span>{t("perms.step", { n: granted ? 2 : 1 })}</span>
        </div>
        <h2>{t(relaunch ? "perms.relaunchTitle" : granted ? "perms.doneTitle" : "perms.title")}</h2>
        <p>{t(relaunch ? "perms.relaunchBody" : granted ? "perms.doneBody" : "perms.body")}</p>
        {row("accessibility", <HandIcon />, t("perms.accessibility"), t("perms.accessibilityHint"))}
        {row("inputMonitoring", <KeyboardIcon />, t("perms.inputMonitoring"), t("perms.inputMonitoringHint"))}
        <div className="onb-foot">
          {granted ? (
            <span className="muted" />
          ) : (
            <button className="btn ghost sm" onClick={onDone}>
              {t("perms.skip")}
            </button>
          )}
          {relaunch ? (
            <button className="btn primary" onClick={() => api.relaunch().catch(console.error)}>
              {t("perms.relaunch")}
            </button>
          ) : (
            <button className={`btn${granted ? " primary" : ""}`} disabled={!granted} onClick={onDone}>
              {t("perms.continue")}
            </button>
          )}
        </div>
        {!granted && <span className="muted">{t("perms.auto")}</span>}
      </div>
    </div>
  );
}
