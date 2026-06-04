"use client";

// /projects/[projectId]/kb — Knowledge Base unificata (scope=project) per
// i membri del progetto. Stesso componente del meta-vault admin, props diverse.
// ADR 0017 v2 fase 7.

import * as React from "react";
import { useParams } from "next/navigation";
import { KnowledgeWorkspace } from "../../../../components/wiki/knowledge-workspace";
import { useThemeColors } from "../../../../lib/theme";
import { useI18n } from "../../../../lib/i18n";

export default function ProjectKbPage() {
  const params = useParams<{ projectId: string }>();
  const projectId = params?.projectId;
  const tc = useThemeColors();
  const { t } = useI18n();

  if (!projectId) {
    return (
      <div style={{ padding: 20, color: tc.error }}>
        Project ID mancante.
      </div>
    );
  }

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        display: "flex",
        flexDirection: "column",
        background: tc.bg,
        zIndex: 5,
      }}
    >
      <div
        style={{
          padding: "10px 16px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          alignItems: "center",
          gap: 12,
        }}
      >
        <a
          href="/ide"
          style={{ color: tc.accent, fontSize: 12, textDecoration: "none" }}
        >
          ← IDE
        </a>
        <h1 style={{ margin: 0, fontSize: 16, fontWeight: 700 }}>
          {t("wiki.title.project")}
        </h1>
        <span style={{ fontSize: 11, color: tc.textSecondary, fontFamily: "monospace" }}>
          {projectId.slice(0, 8)}
        </span>
      </div>
      <div style={{ flex: 1, minHeight: 0, padding: 8 }}>
        <KnowledgeWorkspace scope="project" projectId={projectId} />
      </div>
    </div>
  );
}
