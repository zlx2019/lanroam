// Settings → Transfer, the clipboard part: what of this device's clipboard
// follows the pointer. It belongs to this device's entry in the group (the
// others honour it), so it needs a group.

import { useEffect, useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import type { ClipboardShare, Snapshot } from "../types";
import { Row, Toggle } from "./controls";

/** Everything shared, as a device starts */
const EVERYTHING: ClipboardShare = { on: true, text: true, image: true, files: true };

/** Clipboard sharing, and which kinds */
export function ClipboardSettings({
  snapshot,
  onToast,
}: {
  snapshot: Snapshot;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const local = snapshot.group?.devices.find((d) => d.local) ?? null;
  const synced = local?.clipboard ?? EVERYTHING;
  const [share, setShare] = useState(synced);
  useEffect(() => setShare(synced), [synced.on, synced.text, synced.image, synced.files]);

  /** Change a part, for the group; undone if refused */
  const change = (patch: Partial<ClipboardShare>) => {
    const next = { ...share, ...patch };
    setShare(next);
    api.setClipboard(next).catch((e) => {
      setShare(share);
      onToast(formatError(t, e));
    });
  };

  const noGroup = local ? undefined : t("input.noGroup");
  const kinds = !local || !share.on;
  return (
    <>
      <div className="group-h">{t("clip.title")}</div>
      <div className="group">
        <Row title={t("clip.share")} hint={noGroup}>
          <Toggle on={share.on} label={t("clip.share")} disabled={!local} onChange={(on) => change({ on })} />
        </Row>
        <Row title={t("clip.text")}>
          <Toggle on={share.text} label={t("clip.text")} disabled={kinds} onChange={(text) => change({ text })} />
        </Row>
        <Row title={t("clip.image")}>
          <Toggle on={share.image} label={t("clip.image")} disabled={kinds} onChange={(image) => change({ image })} />
        </Row>
        <Row title={t("clip.files")} hint={t("clip.filesHint")}>
          <Toggle on={share.files} label={t("clip.files")} disabled={kinds} onChange={(files) => change({ files })} />
        </Row>
      </div>
    </>
  );
}
