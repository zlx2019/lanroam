// Settings → Files: whether files are dragged to and from this device, and
// how much of what is copied elsewhere it fetches ahead of a paste. It
// belongs to this device's entry in the group (the others honour it), so it
// needs a group.

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { FileShare, Snapshot } from "../types";
import { Row, Toggle } from "./controls";

/** As a device starts: drags on, fetching ahead up to 1 GiB */
const DEFAULTS: FileShare = { drag: true, prefetch: 1024 };

/** Largest limit that can be set, in MiB (what the engine keeps) */
const MAX_PREFETCH = 4294967295;

/** Dragging files, and the limit of fetching ahead */
export function FilesSettings({ snapshot, onToast }: { snapshot: Snapshot; onToast: (message: string) => void }) {
  const { t } = useI18n();
  const local = snapshot.group?.devices.find((d) => d.local) ?? null;
  const synced = local?.files ?? DEFAULTS;
  const [share, setShare] = useState(synced);
  const [limit, setLimit] = useState(String(synced.prefetch));
  // Esc reverts, and the blur it causes must not save
  const cancelled = useRef(false);
  useEffect(() => {
    setShare(synced);
    setLimit(String(synced.prefetch));
  }, [synced.drag, synced.prefetch]);

  /** Change a part, for the group; undone if refused */
  const change = (patch: Partial<FileShare>) => {
    const next = { ...share, ...patch };
    setShare(next);
    setLimit(String(next.prefetch));
    api.setFiles(next).catch((e) => {
      setShare(share);
      setLimit(String(share.prefetch));
      onToast(formatError(t, e));
    });
  };

  /** Save the limit typed, if it is a whole number of MiB in range */
  const commitLimit = () => {
    if (cancelled.current) {
      cancelled.current = false;
      return;
    }
    const text = limit.trim();
    const mib = Number(text);
    if (!/^\d+$/.test(text) || mib > MAX_PREFETCH) {
      setLimit(String(share.prefetch));
      return;
    }
    if (mib !== share.prefetch) change({ prefetch: mib });
  };

  const noGroup = local ? undefined : t("input.noGroup");
  return (
    <>
      <div className="group">
        <Row title={t("files.drag")} hint={noGroup ?? t("files.dragHint")}>
          <Toggle on={share.drag} label={t("files.drag")} disabled={!local} onChange={(drag) => change({ drag })} />
        </Row>
        <Row title={t("files.prefetch")} hint={t("files.prefetchHint")}>
          <span className="unit-field">
            <input
              className="input"
              inputMode="numeric"
              value={limit}
              disabled={!local}
              aria-label={t("files.prefetch")}
              onChange={(e) => setLimit(e.target.value)}
              onBlur={commitLimit}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") {
                  cancelled.current = true;
                  setLimit(String(share.prefetch));
                  e.currentTarget.blur();
                }
              }}
            />
            MiB
          </span>
        </Row>
      </div>
      <p className="note">{t("files.note")}</p>
    </>
  );
}
