// The devices page: the group's members, the devices nearby, leaving.

import { api } from "../api";
import { useArmed } from "../hooks/useLanroam";
import { formatError, useI18n } from "../i18n";
import type { DeviceDto, NearbyDto, Snapshot } from "../types";
import { PlatformIcon } from "./icons";

/** Armed ID of the leave button (member rows use fingerprints) */
const LEAVE = "leave";

/** The devices page */
export function DevicesPage({
  snapshot,
  nearby,
  onJoin,
  onToast,
}: {
  snapshot: Snapshot;
  nearby: NearbyDto[];
  onJoin: (target: NearbyDto) => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [armed, press] = useArmed();
  const group = snapshot.group;
  const online = group?.devices.filter((d) => d.online).length ?? 0;

  /** Leave the group (second click) */
  const leave = () =>
    press(LEAVE, () => {
      api
        .leaveGroup()
        .then(() => onToast(t("toast.left")))
        .catch((e) => onToast(formatError(t, e)));
    });

  return (
    <div className="pane">
      <div className="pane-inner">
        <div className="sec-h">
          <h3>{t("devices.members")}</h3>
          {group && <span>{t("devices.count", { n: group.devices.length, online })}</span>}
        </div>
        <div className="list">
          {group ? (
            group.devices.map((d) => (
              <MemberRow
                key={d.fingerprint}
                device={d}
                snapshot={snapshot}
                armed={armed === d.fingerprint}
                onKick={() =>
                  press(d.fingerprint, () => {
                    api
                      .kick(d.fingerprint)
                      .then(() => onToast(t("toast.kickedMember", { name: d.name })))
                      .catch((e) => onToast(formatError(t, e)));
                  })
                }
              />
            ))
          ) : (
            <div className="empty">{t("devices.notGrouped")}</div>
          )}
        </div>

        <div className="sec-h">
          <h3>{t("devices.nearby")}</h3>
          <span>{t("devices.nearbyHint")}</span>
        </div>
        <div className="list">
          {nearby.length === 0 ? (
            <div className="empty">{t("devices.nearbyNone")}</div>
          ) : (
            nearby.map((n) => (
              <NearbyRow key={n.fingerprint} device={n} grouped={!!group} onJoin={() => onJoin(n)} />
            ))
          )}
        </div>

        {group && (
          <div className="danger-zone">
            <div>
              <b>{t("devices.leaveTitle")}</b>
              <small>{t("devices.leaveBody")}</small>
            </div>
            <button className={`btn ${armed === LEAVE ? "armed" : "danger"}`} onClick={leave}>
              {t(armed === LEAVE ? "devices.leaveConfirm" : "devices.leave")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** One member */
function MemberRow({
  device,
  snapshot,
  armed,
  onKick,
}: {
  device: DeviceDto;
  snapshot: Snapshot;
  armed: boolean;
  onKick: () => void;
}) {
  const { t } = useI18n();
  const { control } = snapshot;
  const concerned = !device.local && control.peerFingerprint === device.fingerprint;
  let status = device.online ? (
    <span className="tag">{t("tag.online")}</span>
  ) : (
    <span className="tag grey">{t("tag.offline")}</span>
  );
  if (concerned && control.mode === "controlling") {
    status = <span className="tag">{t("tag.controlledByYou")}</span>;
  } else if (concerned && control.mode === "controlled") {
    status = <span className="tag warn">{t("tag.controllingThis")}</span>;
  }
  return (
    <div className="row">
      <div className={`av${device.online ? "" : " off"}`}>
        <PlatformIcon platform={device.platform} />
      </div>
      <div className="who">
        <b>
          {device.name}
          {device.local && <span className="tag">{t("tag.local")}</span>}
        </b>
        <small>
          {t(device.platform === "macos" ? "platform.macos" : "platform.windows")} ·{" "}
          {device.resolution}
          {device.scale !== 100 && ` · ${device.scale}%`}
        </small>
      </div>
      {status}
      {device.local ? (
        <span style={{ width: 64 }} />
      ) : (
        <button className={`btn sm ${armed ? "armed" : "danger"}`} onClick={onKick}>
          {t(armed ? "devices.kickConfirm" : "devices.kick")}
        </button>
      )}
    </div>
  );
}

/** One device nearby */
function NearbyRow({
  device,
  grouped,
  onJoin,
}: {
  device: NearbyDto;
  grouped: boolean;
  onJoin: () => void;
}) {
  const { t } = useI18n();
  let action = <span className="muted">{t("devices.joinThere")}</span>;
  if (!grouped) {
    action = (
      <button className="btn sm primary" onClick={onJoin}>
        {t("devices.join")}
      </button>
    );
  } else if (device.group) {
    action = (
      <button className="btn sm" onClick={onJoin}>
        {t("devices.joinGroup")}
      </button>
    );
  }
  const where = t(device.group ? "devices.otherGroup" : "devices.noGroup");
  return (
    <div className="row">
      <div className="av">
        <PlatformIcon platform={device.platform} />
      </div>
      <div className="who">
        <b>{device.name}</b>
        <small>
          {t(device.platform === "macos" ? "platform.macos" : "platform.windows")}
          {device.address && ` · ${device.address}`} · {where}
        </small>
      </div>
      {action}
    </div>
  );
}
