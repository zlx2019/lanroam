// Settings → Transfer, the files part: whether files are dragged to and
// from this device. It belongs to this device's entry in the group (the
// others honour it), so it needs a group.

import { useEffect, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { FileShare, Snapshot } from "../types";
import { Row, Toggle } from "./controls";

/** As a device starts: drags on */
const DEFAULTS: FileShare = { drag: true };

/** Dragging files */
export function FilesSettings({ snapshot, onToast }: { snapshot: Snapshot; onToast: (message: string) => void }) {
  const { t } = useI18n();
  const local = snapshot.group?.devices.find((d) => d.local) ?? null;
  const synced = local?.files ?? DEFAULTS;
  const [share, setShare] = useState(synced);
  useEffect(() => setShare(synced), [synced.drag]);

  /** Change a part, for the group; undone if refused */
  const change = (patch: Partial<FileShare>) => {
    const next = { ...share, ...patch };
    setShare(next);
    api.setFiles(next).catch((e) => {
      setShare(share);
      onToast(formatError(t, e));
    });
  };

  const noGroup = local ? undefined : t("input.noGroup");
  return (
    <>
      <div className="group-h">{t("files.title")}</div>
      <div className="group">
        <Row title={t("files.drag")} hint={noGroup}>
          <Toggle on={share.drag} label={t("files.drag")} disabled={!local} onChange={(drag) => change({ drag })} />
        </Row>
      </div>
    </>
  );
}
