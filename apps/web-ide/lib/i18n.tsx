"use client";

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

import { translations } from "./i18n/dictionaries";
import type { Locale, TranslationKey } from "./i18n/types";

export type { Locale, TranslationKey } from "./i18n/types";

interface I18nContextValue {
  locale: Locale;
  setLocale: (l: Locale) => void;
  t: (key: TranslationKey, params?: Record<string, string>) => string;
}

const I18nContext = createContext<I18nContextValue>({
  locale: "en",
  setLocale: () => {},
  t: (key) => key,
});

export function useI18n() {
  return useContext(I18nContext);
}

export const LOCALE_LABELS: Record<Locale, string> = {
  en: "English",
  it: "Italiano",
  es: "Español",
};

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>("en");

  useEffect(() => {
    const saved = localStorage.getItem("nexus-locale") as Locale | null;
    if (saved && saved in translations) {
      setLocaleState(saved);
    }
  }, []);

  const setLocale = (l: Locale) => {
    setLocaleState(l);
    localStorage.setItem("nexus-locale", l);
  };

  const t = (key: TranslationKey, params?: Record<string, string>): string => {
    let text = (translations[locale] as Record<string, string>)[key] || (translations.en as Record<string, string>)[key] || key;
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        text = text.replace(`{${k}}`, v);
      }
    }
    return text;
  };

  return (
    <I18nContext.Provider value={{ locale, setLocale, t }}>
      {children}
    </I18nContext.Provider>
  );
}
