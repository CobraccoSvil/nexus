"use client";

// /projects/[projectId]/kb — Knowledge Base unificata (scope=project) per
// i membri del progetto. Stesso componente del meta-vault admin, props diverse.
// ADR 0017 v2 fase 7.

import * as React from "react";
import { useParams, useSearchParams } from "next/navigation";
import { KnowledgeWorkspace } from "../../../../components/wiki/knowledge-workspace";
import { useThemeColors } from "../../../../lib/theme";
import { useI18n } from "../../../../lib/i18n";
import { CobraccoMark } from "../../../../components/landing/CobraccoMark";

export default function ProjectKbPage() {
  const params = useParams<{ projectId: string }>();
  const projectId = params?.projectId;
  // `?doc=<id>` permette al navigatore leggero della sidebar IDE di aprire la
  // KB completa (3 colonne) gia' posizionata sul documento selezionato.
  const searchParams = useSearchParams();
  const initialDocId = searchParams?.get("doc") ?? undefined;
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
          flexShrink: 0,
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
      <div style={{ flex: 1, minHeight: 0, display: "flex", overflow: "hidden", padding: 8 }}>
        <KnowledgeWorkspace scope="project" projectId={projectId} initialDocId={initialDocId} />
      </div>
      {/* Footer Nexus: coerente con landing/pricing (copyright + CobraccoMark). */}
      <footer
        style={{
          flexShrink: 0,
          padding: "8px 16px",
          borderTop: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          gap: 6,
          fontSize: 11,
          color: tc.textSecondary,
        }}
      >
        <span>
          &copy; 2026 {t("landing.v2.footer.copyright")}{" "}
          <a
            href="https://cobracco.it"
            target="_blank"
            rel="noopener noreferrer"
            aria-label="Cobracco"
            style={{ color: "inherit", textDecoration: "underline" }}
          >
            <CobraccoMark />
          </a>
        </span>
      </footer>
    </div>
  );
}
