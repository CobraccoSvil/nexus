"use client";

import { useI18n } from "../../lib/i18n";

export function GraphTab() {
  const { t } = useI18n();
  return (
    <div style={{ padding: 32, textAlign: "center" }}>
      <div style={{ fontSize: 32, marginBottom: 12, opacity: 0.3 }}>&#9679;&#8212;&#9679;</div>
      <p style={{ fontSize: 13, color: "#a3a3a3", fontStyle: "italic" }}>
        {t("knowledge.graph.placeholder")}
      </p>
    </div>
  );
}
