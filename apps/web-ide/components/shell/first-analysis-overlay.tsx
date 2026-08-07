"use client";

import type { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";

export function FirstAnalysisOverlay({
  tc,
  analysisInProgress,
  analysisStep,
  onAnalyze,
  onSkip,
}: {
  tc: ReturnType<typeof useThemeColors>;
  analysisInProgress: boolean;
  analysisStep: string;
  onAnalyze: () => void;
  onSkip: () => void;
}) {
  const { t } = useI18n();
  return (
    <div
      style={{
        gridRow: "2 / 5",
        gridColumn: "1 / 5",
        zIndex: 100,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: tc.bg,
      }}
    >
      <div style={{
        maxWidth: 520,
        textAlign: "center",
        padding: 40,
        borderRadius: 12,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        boxShadow: "0 8px 32px rgba(0,0,0,0.18)",
      }}>
        <div style={{ fontSize: 36, marginBottom: 16 }}>
          {analysisInProgress ? "⚙️" : "📂"}
        </div>
        <div style={{ fontSize: 16, fontWeight: 700, color: tc.text, marginBottom: 8 }}>
          {analysisInProgress ? "Analisi in corso..." : "Progetto non analizzato"}
        </div>
        <div style={{ fontSize: 13, color: tc.textSecondary, marginBottom: 20, lineHeight: "1.5" }}>
          {analysisInProgress
            ? analysisStep || "Nexus sta analizzando la struttura del progetto, i linguaggi, i framework e le configurazioni di esecuzione."
            : "Nexus deve analizzare il progetto prima di poter offrire le funzionalita' complete (servizi, comandi, diagnostica, AI contestuale)."}
        </div>
        {analysisInProgress ? (
          <div style={{
            height: 4,
            borderRadius: 2,
            background: tc.border,
            overflow: "hidden",
            marginBottom: 12,
          }}>
            <div style={{
              height: "100%",
              background: tc.accent,
              borderRadius: 2,
              animation: "nexus-analysis-progress 2s ease-in-out infinite",
              width: "40%",
            }} />
            <style>{`
              @keyframes nexus-analysis-progress {
                0% { transform: translateX(-100%); }
                100% { transform: translateX(350%); }
              }
            `}</style>
          </div>
        ) : (
          <button
            type="button"
            onClick={onAnalyze}
            style={{
              background: tc.accent,
              color: "#fff",
              border: "none",
              borderRadius: 8,
              padding: "10px 28px",
              fontSize: 14,
              fontWeight: 600,
              cursor: "pointer",
              fontFamily: "inherit",
            }}
          >
            {t("shell.analizzaProgetto")}
          </button>
        )}
        {!analysisInProgress && (
          <div style={{ marginTop: 12 }}>
            <button
              type="button"
              onClick={onSkip}
              style={{
                background: "transparent",
                color: tc.textMuted,
                border: "none",
                fontSize: 11,
                cursor: "pointer",
                textDecoration: "underline",
                fontFamily: "inherit",
              }}
            >
              {t("shell.saltaEContinuaSenza")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
