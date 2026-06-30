"use client";

import { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";

export default function AdminPage() {
  const tc = useThemeColors();
  const { t } = useI18n();

  return (
    <div>
      <h1 style={{ fontSize: 20, fontWeight: 600, marginBottom: 6 }}>{t("admin.settings")}</h1>
      <p style={{ color: tc.textMuted, fontSize: 13, marginBottom: 28 }}>
        {t("admin.settings.select")}
      </p>
    </div>
  );
}
