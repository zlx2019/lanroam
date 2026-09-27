// Interface language: the settings' choice, or the system's.

import { createContext, useCallback, useContext, useMemo, type ReactNode } from "react";
import { en } from "./en";
import { zh, type TextKey } from "./zh";
import type { CommandError } from "../types";

/** Languages of the interface */
export type Lang = "zh" | "en";

/** Translate a key, filling {placeholders} */
export type Translate = (key: TextKey, vars?: Record<string, string | number>) => string;

/** The language for a setting (`system` follows the OS) */
export function resolveLang(setting: string | undefined): Lang {
  if (setting === "zh" || setting === "en") return setting;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

/** Fill {name} placeholders */
function fill(text: string, vars?: Record<string, string | number>): string {
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (all, name: string) =>
    name in vars ? String(vars[name]) : all,
  );
}

const I18nContext = createContext<{ lang: Lang; t: Translate }>({
  lang: "zh",
  t: (key, vars) => fill(zh[key], vars),
});

/** Provides the texts of `lang` to the tree */
export function I18nProvider({ lang, children }: { lang: Lang; children: ReactNode }) {
  const t = useCallback<Translate>(
    (key, vars) => fill((lang === "zh" ? zh : en)[key] ?? key, vars),
    [lang],
  );
  const value = useMemo(() => ({ lang, t }), [lang, t]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

/** The current language and translator */
export function useI18n() {
  return useContext(I18nContext);
}

/** A failed command in the user's words: its code's text, or the engine's message */
export function formatError(t: Translate, error: unknown): string {
  const e = error as Partial<CommandError> | undefined;
  const key = `error.${e?.code ?? ""}` as TextKey;
  if (e?.code && key in zh) return t(key);
  return t("error.unknown", { message: e?.message ?? String(error) });
}
