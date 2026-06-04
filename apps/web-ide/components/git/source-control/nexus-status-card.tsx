"use client";

import type { UserProjectDetails } from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { cardStyle, smallButtonStyle, statusBadgeStyle } from "./styles";
import { readinessTone } from "./labels";

interface NexusStatusCardProps {
  project: UserProjectDetails;
  isNexusReady: boolean;
  busy: boolean;
  githubBusy: boolean;
  analyzeBusy: boolean;
  deepAnalysisPhase: "idle" | "static" | "deep";
  onAnalyzeProject: () => void;
}

export function NexusStatusCard({
  project,
  isNexusReady,
  busy,
  githubBusy,
  analyzeBusy,
  deepAnalysisPhase,
  onAnalyzeProject,
}: NexusStatusCardProps) {
  const tc = useThemeColors();

  return (
    <div style={cardStyle(tc)}>
      <div style={{ display: "flex", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <div style={{ color: tc.text, fontWeight: 700 }}>Stato Progetto Nexus</div>
          <div style={{ color: tc.textSecondary }}>
            {isNexusReady
              ? "Nexus Ready: progetto analizzato e pronto alla gestione AI."
              : "Analisi richiesta: progetto non inizializzato per Nexus."}
          </div>
          <div style={{ color: tc.textMuted, fontSize: 12 }}>
            Puoi inizializzare il progetto in qualsiasi momento.
          </div>
          {project.analyzedAt ? (
            <div style={{ color: tc.textMuted, fontSize: 11 }}>
              Ultima analisi: {new Date(project.analyzedAt).toLocaleString()}
            </div>
          ) : null}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          <span style={statusBadgeStyle(tc, readinessTone(isNexusReady))}>
            {isNexusReady ? "Nexus Ready" : "Analisi richiesta"}
          </span>
          <button
            disabled={busy || githubBusy || analyzeBusy}
            onClick={() => void onAnalyzeProject()}
            style={smallButtonStyle(tc, busy || githubBusy || analyzeBusy)}
          >
            {analyzeBusy
              ? (deepAnalysisPhase === "deep" ? "Analisi AI..." : "Analisi statica...")
              : isNexusReady ? "Rianalizza progetto" : "Analizza progetto"}
          </button>
        </div>
      </div>
    </div>
  );
}
