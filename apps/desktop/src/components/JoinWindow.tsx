// The window on the device asked to let another one in: the PIN to type
// there, the attempts left, and a way to decline.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import { EVENTS } from "../events";
import { useI18n } from "../i18n";
import type { JoinEndedDto, JoinPromptDto } from "../types";
import { CheckIcon, CrossIcon, PlatformIcon } from "./icons";

/** How long the outcome stays before the window hides */
const OUTCOME_MS = 1800;

/** The join window */
export function JoinWindow() {
  const { t } = useI18n();
  const [prompt, setPrompt] = useState<JoinPromptDto | null>(null);
  const [ended, setEnded] = useState<JoinEndedDto | null>(null);

  useEffect(() => {
    let timer: number | undefined;
    api.getJoinPrompt().then(setPrompt).catch(console.error);
    const unlistenPrompt = listen<JoinPromptDto>(EVENTS.JOIN_PROMPT, (e) => {
      clearTimeout(timer);
      setEnded(null);
      setPrompt(e.payload);
    });
    const unlistenEnded = listen<JoinEndedDto>(EVENTS.JOIN_ENDED, (e) => {
      setEnded(e.payload);
      timer = window.setTimeout(() => {
        getCurrentWindow().hide().catch(console.error);
        setPrompt(null);
        setEnded(null);
      }, OUTCOME_MS);
    });
    return () => {
      clearTimeout(timer);
      unlistenPrompt.then((u) => u()).catch(console.error);
      unlistenEnded.then((u) => u()).catch(console.error);
    };
  }, []);

  if (ended) {
    return (
      <div className="prompt" data-tauri-drag-region>
        <div className={`big-ic${ended.admitted ? "" : " bad"}`}>
          {ended.admitted ? <CheckIcon /> : <CrossIcon />}
        </div>
        <b style={{ fontSize: 15 }}>
          {ended.admitted ? t("prompt.admitted", { name: ended.name }) : t("prompt.ended")}
        </b>
      </div>
    );
  }
  if (!prompt) return <div className="prompt" data-tauri-drag-region />;
  const [head, tail] = [prompt.pin.slice(0, 3), prompt.pin.slice(3)];
  return (
    <div className="prompt" data-tauri-drag-region>
      <div className="av">
        <PlatformIcon platform={prompt.platform} />
      </div>
      <b style={{ fontSize: 15 }}>{t("prompt.title", { name: prompt.name })}</b>
      <p>
        {t(prompt.platform === "macos" ? "platform.macos" : "platform.windows")}
        {prompt.address && ` · ${prompt.address}`}
      </p>
      <div className="bigpin">
        {head}
        <span>·</span>
        {tail}
      </div>
      <p>{t("prompt.body", { name: prompt.name })}</p>
      <span className="left-t">{t("prompt.left", { n: prompt.attemptsLeft })}</span>
      <button className="btn danger wide" style={{ marginTop: 6 }} onClick={() => api.rejectJoin()}>
        {t("prompt.reject")}
      </button>
    </div>
  );
}
