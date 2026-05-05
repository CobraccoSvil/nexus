"use client";

import { useThemeColors } from "../../../lib/theme";
import { useI18n, LOCALE_LABELS, type Locale } from "../../../lib/i18n";

const locales: { code: Locale; flag: string }[] = [
  { code: "en", flag: "EN" },
  { code: "it", flag: "IT" },
  { code: "es", flag: "ES" },
];

export default function LanguagePage() {
  const tc = useThemeColors();
  const { locale, setLocale, t } = useI18n();

  return (
    <div>
      <h1 style={{ fontSize: 20, fontWeight: 600, marginBottom: 6 }}>{t("admin.language")}</h1>
      <p style={{ color: tc.textMuted, fontSize: 13, marginBottom: 28 }}>
        {t("admin.language.desc")}
      </p>

      <div style={{ display: "flex", gap: 16 }}>
        {locales.map((loc) => {
          const active = locale === loc.code;
          return (
            <button
              key={loc.code}
              onClick={() => setLocale(loc.code)}
              style={{
                flex: 1,
                padding: "24px 20px",
                borderRadius: 12,
                border: `2px solid ${active ? tc.accent : tc.border}`,
                background: active ? tc.accentBg : tc.bgCard,
                color: tc.text,
                cursor: "pointer",
                textAlign: "center",
                fontFamily: "inherit",
                transition: "all 0.2s",
              }}
            >
              <div style={{ fontSize: 28, marginBottom: 8 }}>{loc.flag}</div>
              <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 4 }}>
                {LOCALE_LABELS[loc.code]}
              </div>
              {active && (
                <div
                  style={{
                    marginTop: 12,
                    padding: "4px 12px",
                    borderRadius: 6,
                    background: tc.accent,
                    color: "#fff",
                    fontSize: 11,
                    fontWeight: 600,
                    display: "inline-block",
                  }}
                >
                  {t("theme.active")}
                </div>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
