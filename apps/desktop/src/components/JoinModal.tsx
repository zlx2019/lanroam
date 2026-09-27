// Joining a desk group through a nearby device: (leave the current group,)
// connect, type the PIN it shows, done.

import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import { formatError, useI18n } from "../i18n";
import type { TextKey } from "../i18n/zh";
import type { JoiningEndedDto, NearbyDto } from "../types";
import { CheckIcon, CrossIcon } from "./icons";

/** Where the join stands */
type Step = "confirm" | "connecting" | "pin" | "done" | "failed";

/** The join dialog for `target` */
export function JoinModal({
  target,
  grouped,
  onClose,
  onJoined,
}: {
  target: NearbyDto;
  /** This device is in a group: it has to leave first */
  grouped: boolean;
  onClose: () => void;
  onJoined: () => void;
}) {
  const { t } = useI18n();
  const [step, setStep] = useState<Step>(grouped ? "confirm" : "connecting");
  const [left, setLeft] = useState(3);
  const [pin, setPin] = useState("");
  const [error, setError] = useState("");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState(false);
  // Closed while still connecting: a join that starts afterwards is dropped
  const closed = useRef(false);
  const input = useRef<HTMLInputElement>(null);

  /** Connect to the target; it shows its PIN */
  const connect = () => {
    setStep("connecting");
    api
      .startJoin(target.fingerprint)
      .then((started) => {
        if (closed.current) {
          api.cancelJoin().catch(console.error);
          return;
        }
        setLeft(started.attemptsLeft);
        setStep("pin");
      })
      .catch((e) => fail(formatError(t, e)));
  };

  /** End with a failure message */
  const fail = (message: string) => {
    setFailure(message);
    setStep("failed");
  };

  // Not in a group: connect right away
  useEffect(() => {
    if (!grouped) connect();
    return () => {
      closed.current = true;
    };
    // Once per dialog: it is opened for one target
  }, []);

  // The sponsor may turn the join down, or time out, while the PIN is typed
  useEffect(() => {
    const unlisten = listen<JoiningEndedDto>(EVENTS.JOINING_ENDED, (e) => {
      const reason = e.payload.reason;
      const key = `error.${reason ?? "ended"}` as TextKey;
      fail(t(key) === key ? t("error.ended") : t(key));
    });
    return () => {
      unlisten.then((u) => u()).catch(console.error);
    };
  }, [t]);

  useEffect(() => {
    if (step === "pin") input.current?.focus();
  }, [step, error]);

  /** Leave the current group, then join the target's */
  const leaveAndJoin = () => {
    api
      .leaveGroup()
      .then(connect)
      .catch((e) => fail(formatError(t, e)));
  };

  /** Try the PIN typed */
  const submit = () => {
    const digits = pin.replace(/\D/g, "");
    if (digits.length !== 6) {
      setError(t("error.pin_format"));
      return;
    }
    setBusy(true);
    api
      .answerJoin(digits)
      .then((answer) => {
        if (answer.joined) {
          setStep("done");
          return;
        }
        setLeft(answer.attemptsLeft);
        setPin("");
        setError(t("join.wrong", { n: answer.attemptsLeft }));
      })
      .catch((e) => fail(formatError(t, e)))
      .finally(() => setBusy(false));
  };

  /** Give up */
  const cancel = () => {
    closed.current = true;
    if (step === "connecting" || step === "pin") api.cancelJoin().catch(console.error);
    onClose();
  };

  const name = target.name;
  return (
    <div className="scrim">
      <div className="modal">
        {step === "confirm" && (
          <>
            <h3>{t("join.confirmTitle", { name })}</h3>
            <p>{t("join.confirmBody")}</p>
            <div className="acts">
              <button className="btn" onClick={cancel}>
                {t("cancel")}
              </button>
              <button className="btn primary" onClick={leaveAndJoin}>
                {t("join.leaveAndJoin")}
              </button>
            </div>
          </>
        )}
        {step === "connecting" && (
          <>
            <div className="center">
              <div className="spin" />
              <h3>{t("join.connecting", { name })}</h3>
              <p>{t("join.connectingHint")}</p>
            </div>
            <div className="acts">
              <button className="btn" onClick={cancel}>
                {t("cancel")}
              </button>
            </div>
          </>
        )}
        {step === "pin" && (
          <>
            <h3>{t("join.pinTitle")}</h3>
            <p>{t("join.pinBody", { name })}</p>
            <input
              ref={input}
              className={`pin${error ? " err" : ""}`}
              inputMode="numeric"
              maxLength={7}
              placeholder="······"
              autoComplete="off"
              value={pin}
              disabled={busy}
              onChange={(e) => {
                setPin(e.target.value);
                setError("");
              }}
              onKeyDown={(e) => e.key === "Enter" && submit()}
            />
            {error ? <span className="err-t">{error}</span> : <span className="left-t">{t("join.left", { n: left })}</span>}
            <div className="acts">
              <button className="btn" onClick={cancel}>
                {t("cancel")}
              </button>
              <button className="btn primary" onClick={submit} disabled={busy}>
                {t("join.submit")}
              </button>
            </div>
          </>
        )}
        {step === "done" && (
          <>
            <div className="center">
              <div className="big-ic">
                <CheckIcon />
              </div>
              <h3>{t("join.doneTitle")}</h3>
              <p>{t("join.doneBody", { name })}</p>
            </div>
            <div className="acts mid">
              <button className="btn primary" onClick={onJoined}>
                {t("ok")}
              </button>
            </div>
          </>
        )}
        {step === "failed" && (
          <>
            <div className="center">
              <div className="big-ic bad">
                <CrossIcon />
              </div>
              <h3>{t("join.failedTitle")}</h3>
              <p>{failure}</p>
            </div>
            <div className="acts mid">
              <button className="btn" onClick={onClose}>
                {t("ok")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
