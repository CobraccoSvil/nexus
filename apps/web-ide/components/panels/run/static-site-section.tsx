"use client";

import { useEffect, useState } from "react";
import type { useThemeColors } from "../../../lib/theme";
import { hdrStyle } from "./shared";

// Risposta dell'endpoint protetto mcp-core GET /api/projects/:id/static-site.
// Quando detected e' true, entry e url sono valorizzati.
interface StaticSiteResponse {
  detected: boolean;
  entry?: string;
  url?: string;
}

interface StaticSiteSectionProps {
  tc: ReturnType<typeof useThemeColors>;
  projectId: string;
}

// Sezione del pannello SERVIZI che rileva un sito statico HTML nella project_root
// (presenza di index.html) e offre un pulsante per aprirlo in una nuova scheda.
// Il sito viene servito da mcp-core via la route pubblica /preview, proxata
// same-origin da apps/web-ide/app/preview/[projectId]/[[...path]]/route.ts.
export function StaticSiteSection({ tc, projectId }: StaticSiteSectionProps) {
  const [site, setSite] = useState<StaticSiteResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    setSite(null);
    (async () => {
      try {
        const res = await fetch(`/api/projects/${projectId}/static-site`);
        if (!res.ok) return;
        const data = (await res.json()) as StaticSiteResponse;
        if (!cancelled) setSite(data);
      } catch {
        // Silenzioso: in assenza di sito statico la sezione semplicemente non compare.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  // Se non rilevato (o non ancora caricato), non ingombrare il pannello.
  if (!site?.detected || !site.url) return null;

  const openInBrowser = () => {
    window.open(site.url, "_blank", "noopener,noreferrer");
  };

  return (
    <>
      <div
        style={hdrStyle(tc)}
        title="Sito statico HTML rilevato nella cartella del progetto, servito dal server integrato di Nexus."
      >
        <span>Sito statico HTML</span>
      </div>
      <div style={{ padding: "8px 12px", borderBottom: `1px solid ${tc.border}` }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
          <span style={{ color: "#22c55e", fontSize: 13, flexShrink: 0 }}>●</span>
          <span
            title={site.entry}
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: 12,
              color: tc.text,
              fontFamily: '"JetBrains Mono", monospace',
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {site.entry ?? "index.html"}
          </span>
          <button
            type="button"
            onClick={openInBrowser}
            title="Apri il sito statico in una nuova scheda del browser"
            style={{
              flexShrink: 0,
              background: "#22c55e",
              color: "#fff",
              border: "none",
              borderRadius: 4,
              padding: "3px 12px",
              fontSize: 11,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            Apri nel browser
          </button>
        </div>
      </div>
    </>
  );
}
