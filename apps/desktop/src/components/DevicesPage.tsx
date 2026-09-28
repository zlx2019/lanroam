// The devices page: the group's members as cards (their screens, where the
// pointer is, a jump there, removing or leaving), and the devices nearby;
// outside a group, the devices to join.

import { useEffect, useState } from "react";
import { api } from "../api";
import { fit } from "../geometry";
import { useArmed } from "../hooks/useLanroam";
import { formatError, useI18n } from "../i18n";
import type { DeviceDto, NearbyDto, Snapshot } from "../types";
import { MoreIcon, OutIcon, PlatformIcon, WarnIcon } from "./icons";
import { Screens } from "./Screens";

/** Size of a card's picture of the screens (px), and the room around them */
const SHOT = { w: 150, h: 96, padding: 12 };

/** The devices page */
export function DevicesPage({
  snapshot,
  nearby,
  onJoin,
  onRename,
  onToast,
}: {
  snapshot: Snapshot;
  nearby: NearbyDto[];
  onJoin: (target: NearbyDto) => void;
  /** Go and rename this device */
  onRename: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const { group, input } = snapshot;
  const problem = input.capture
    ? t("input.captureOff", { reason: input.capture })
    : input.injection
      ? t("input.injectionOff", { reason: input.injection })
      : null;
  return (
    <div className="pane">
      {problem && (
        <div className="banner warn">
          <WarnIcon />
          <span>{problem}</span>
        </div>
      )}
      {group ? (
        <Members snapshot={snapshot} devices={group.devices} onRename={onRename} onToast={onToast} />
      ) : (
        <div className="hero-empty">
          <div className="radar">
            <i />
            <i />
            <b />
          </div>
          <h3>{t("devices.emptyTitle")}</h3>
          <p>{t("devices.emptyBody")}</p>
        </div>
      )}
      <div className="sec-h">
        {t("devices.nearby")} <span className="scan" />
      </div>
      {nearby.length === 0 ? (
        <div className="empty-note">{t("devices.nearbyNone")}</div>
      ) : (
        nearby.map((n) => <NearbyRow key={n.fingerprint} device={n} grouped={!!group} onJoin={() => onJoin(n)} />)
      )}
    </div>
  );
}

/** The group's members, by number */
function Members({
  snapshot,
  devices,
  onRename,
  onToast,
}: {
  snapshot: Snapshot;
  devices: DeviceDto[];
  onRename: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [menu, setMenu] = useState<string | null>(null);
  const online = devices.filter((d) => d.online).length;
  // Where the pointer is: the device it went to (while that is online), or this one
  const pointer = devices.find((d) => d.fingerprint === snapshot.control.pointer && d.online);
  const here = pointer?.fingerprint ?? snapshot.device.fingerprint;

  // A press outside the open menu closes it
  useEffect(() => {
    if (!menu) return;
    const away = (e: PointerEvent) => {
      const el = e.target instanceof Element ? e.target : null;
      if (!el?.closest(".menu, .more")) setMenu(null);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [menu]);

  return (
    <>
      <div className="sec-h">
        {t("devices.group")} <span className="m">· {t("devices.online", { online, n: devices.length })}</span>
      </div>
      <div className="cards">
        {devices.map((d) => (
          <Card
            key={d.fingerprint}
            device={d}
            here={d.fingerprint === here}
            menu={menu === d.fingerprint}
            onMenu={() => setMenu(menu === d.fingerprint ? null : d.fingerprint)}
            onRename={() => {
              setMenu(null);
              onRename();
            }}
            onDone={() => setMenu(null)}
            onToast={onToast}
          />
        ))}
      </div>
    </>
  );
}

/** One member: its screens, what it is, where the pointer is */
function Card({
  device,
  here,
  menu,
  onMenu,
  onRename,
  onDone,
  onToast,
}: {
  device: DeviceDto;
  /** The pointer is on it */
  here: boolean;
  /** Its menu is open */
  menu: boolean;
  onMenu: () => void;
  onRename: () => void;
  /** Its menu did its job */
  onDone: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const view = fit(device.screens, SHOT.w, SHOT.h, SHOT.padding, 1);

  /** Move control there, at the next local input */
  const jump = () =>
    api.requestAction({ kind: "jump", fingerprint: device.fingerprint }).catch((e) => onToast(formatError(t, e)));

  const system = t(device.platform === "macos" ? "platform.macos" : "platform.windows");
  const displays = device.displays > 1 ? ` · ${t("devices.displays", { n: device.displays })}` : "";
  const status = device.local ? t("tag.local") : device.online ? t("tag.online") : t("tag.offline");
  return (
    <div className={`card${here ? " here" : ""}${device.online ? "" : " offline"}`}>
      <div className="shot">
        {view && <Screens device={device} screens={device.screens} view={view} labels={false} />}
      </div>
      <div className="info">
        <b>
          <span>{device.name}</span>
          {here && <span className="tag accent">{t("tag.inUse")}</span>}
        </b>
        <small>
          {system}
          {displays}
          {device.resolution && ` · ${device.resolution}`}
        </small>
        <div className="row">
          <span className={`dot${device.online ? " live" : ""}`} />
          {status}
          <span className="acts">
            {!device.local && device.online && !here && (
              <button className="btn icon" onClick={jump} title={t("devices.jump")}>
                <OutIcon />
              </button>
            )}
            <button className="btn icon ghost more" onClick={onMenu} title={t("devices.more")}>
              <MoreIcon />
            </button>
          </span>
        </div>
      </div>
      {device.number !== null && <span className="num-badge">{device.number}</span>}
      {menu && <Menu device={device} onRename={onRename} onDone={onDone} onToast={onToast} />}
    </div>
  );
}

/** A member's actions: this device is renamed or leaves, another one is
 * removed; both of those take a second click */
function Menu({
  device,
  onRename,
  onDone,
  onToast,
}: {
  device: DeviceDto;
  onRename: () => void;
  onDone: () => void;
  onToast: (message: string) => void;
}) {
  const { t } = useI18n();
  const [armed, press] = useArmed();

  /** Leave the group, or remove the member (second click) */
  const remove = () =>
    press(device.fingerprint, () => {
      onDone();
      const done = device.local ? api.leaveGroup() : api.kick(device.fingerprint);
      done
        .then(() => onToast(device.local ? t("toast.left") : t("toast.kickedMember", { name: device.name })))
        .catch((e) => onToast(formatError(t, e)));
    });

  const confirm = armed === device.fingerprint;
  return (
    <div className="menu">
      {device.local && <button onClick={onRename}>{t("devices.rename")}</button>}
      <button className={`danger${confirm ? " armed" : ""}`} onClick={remove}>
        {device.local
          ? t(confirm ? "devices.leaveConfirm" : "devices.leave")
          : t(confirm ? "devices.kickConfirm" : "devices.kick")}
      </button>
    </div>
  );
}

/** One device nearby */
function NearbyRow({ device, grouped, onJoin }: { device: NearbyDto; grouped: boolean; onJoin: () => void }) {
  const { t } = useI18n();
  let action = <span className="muted">{t("devices.joinThere")}</span>;
  if (!grouped) {
    action = (
      <button className="btn accent" onClick={onJoin}>
        {t("devices.join")}
      </button>
    );
  } else if (device.group) {
    action = (
      <button className="btn" onClick={onJoin}>
        {t("devices.joinGroup")}
      </button>
    );
  }
  return (
    <div className="near">
      <span className="glyph">
        <PlatformIcon platform={device.platform} />
      </span>
      <div className="who">
        <b>{device.name}</b>
        {device.address && <small>{device.address}</small>}
      </div>
      {action}
    </div>
  );
}
