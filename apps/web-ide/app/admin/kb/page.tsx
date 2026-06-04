"use client";

// /admin/kb — Knowledge Base unificata (scope=meta) per amministratori Nexus.
// Sostituisce /admin/nexus-docs (che resta come pagina deprecata con banner).
// Usa KnowledgeWorkspace scope-agnostic + endpoint /api/wiki/* (ADR 0017 v2).

import * as React from "react";
import { KnowledgeWorkspace } from "../../../components/wiki/knowledge-workspace";
import { useThemeColors } from "../../../lib/theme";
import { useI18n } from "../../../lib/i18n";

export default function AdminKbPage() {
  const tc = useThemeColors();
  const { t } = useI18n();

  // Il layout admin impone maxWidth 900px sul main: per rendere il workspace
  // a piena pagina usiamo position: fixed sotto la header. NON modifichiamo
  // admin/layout.tsx (regola: modifiche chirurgiche).
  return (
    <div
      style={{
        position: "fixed",
        top: 57,
        left: 0,
        right: 0,
        bottom: 0,
        display: "flex",
        flexDirection: "column",
        background: tc.bg,
        zIndex: 5,
      }}
    >
      <div
        style={{
          flexShrink: 0,
          padding: "10px 16px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          alignItems: "center",
          gap: 12,
        }}
      >
        <h1 style={{ margin: 0, fontSize: 16, fontWeight: 700 }}>
          {t("wiki.title.meta")}
        </h1>
        <span style={{ fontSize: 11, color: tc.textSecondary }}>
          scope=meta
        </span>
      </div>
      <div style={{ flex: 1, minHeight: 0, display: "flex", overflow: "hidden", padding: 8 }}>
        <KnowledgeWorkspace scope="meta" />
      </div>
    </div>
  );
}
