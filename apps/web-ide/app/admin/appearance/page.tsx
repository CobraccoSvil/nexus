"use client";

import { useTheme, useThemeColors, type ThemeMode } from "../../../lib/theme";
import { useI18n } from "../../../lib/i18n";

export default function AppearancePage() {
  const { mode, setMode } = useTheme();
  const tc = useThemeColors();
  const { t } = useI18n();

  const options: { mode: ThemeMode; labelKey: Parameters<typeof t>[0]; descKey: Parameters<typeof t>[0]; icon: string }[] = [
    { mode: "light", labelKey: "theme.light", descKey: "theme.light.desc", icon: "☀" },
    { mode: "dark", labelKey: "theme.dark", descKey: "theme.dark.desc", icon: "☾" },
    { mode: "auto", labelKey: "theme.auto", descKey: "theme.auto.desc", icon: "◐" },
  ];

  return (
    <div>
      <h1 style={{ fontSize: 20, fontWeight: 600, marginBottom: 6 }}>{t("admin.appearance")}</h1>
      <p style={{ color: tc.textMuted, fontSize: 13, marginBottom: 28 }}>
        {t("admin.appearance.desc")}
      </p>

      <div style={{ display: "flex", gap: 16 }}>
        {options.map((opt) => {
          const active = mode === opt.mode;
          return (
            <button
              key={opt.mode}
              onClick={() => setMode(opt.mode)}
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
              <div style={{ fontSize: 28, marginBottom: 8 }}>{opt.icon}</div>
              <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 4 }}>{t(opt.labelKey)}</div>
              <div style={{ fontSize: 12, color: tc.textMuted }}>{t(opt.descKey)}</div>
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
