"use client";

import type { QualityFinding, QualityScanResult } from "../../../lib/api-client";
import type { Tc, FixQueueItem } from "./types";

interface OptimizationToolbarProps {
  tc: Tc;
  scanning: boolean;
  depsOk: boolean;
  onSendToChat?: (message: string) => void;
  scanResult: QualityScanResult | null;
  allActiveFindings: QualityFinding[];
  highCount: number;
  mediumCount: number;
  lowCount: number;
  fixQueue: FixQueueItem[];
  fixQueueIndex: number;
  autoFixEnabled: boolean;
  setAutoFixEnabled: (v: boolean) => void;
  storageKey: string;
  setFixQueue: (v: FixQueueItem[]) => void;
  setFixQueueIndex: (v: number) => void;
  handleScan: () => void;
  startFixQueue: (targetFindings: QualityFinding[], autoFix?: boolean) => void;
  handleFixNext: () => void;
  // Deep review
  deepReviewSubmitting: boolean;
  deepReviewState: string | null;
  deepReviewCompleted: number;
  deepReviewTotal: number;
  deepReviewError: string | null;
  deepReviewJobId: string | null;
  handleDeepReview: () => void;
  stopDeepReviewPoll: () => void;
  setDeepReviewJobId: (v: string | null) => void;
  setDeepReviewState: (v: string | null) => void;
  setDeepReviewError: (v: string | null) => void;
  setDeepReviewCompleted: (v: number) => void;
  setDeepReviewTotal: (v: number) => void;
  pollDeepReviewStatus: (jobId: string) => void;
}

export function OptimizationToolbar({
  tc,
  scanning,
  depsOk,
  onSendToChat,
  scanResult,
  allActiveFindings,
  highCount,
  mediumCount,
  lowCount,
  fixQueue,
  fixQueueIndex,
  autoFixEnabled,
  setAutoFixEnabled,
  storageKey,
  setFixQueue,
  setFixQueueIndex,
  handleScan,
  startFixQueue,
  handleFixNext,
  deepReviewSubmitting,
  deepReviewState,
  deepReviewCompleted,
  deepReviewTotal,
  deepReviewError,
  deepReviewJobId,
  handleDeepReview,
  stopDeepReviewPoll,
  setDeepReviewJobId,
  setDeepReviewState,
  setDeepReviewError,
  setDeepReviewCompleted,
  setDeepReviewTotal,
  pollDeepReviewStatus,
}: OptimizationToolbarProps) {
  return (
    <div style={{
      display: "flex", alignItems: "center", gap: 8, padding: "6px 10px",
      borderBottom: `1px solid ${tc.border}`, flexShrink: 0, flexWrap: "wrap",
    }}>
      <button
        onClick={handleScan}
        disabled={scanning}
        style={{
          background: tc.accent, color: "#fff", border: "none", borderRadius: 6,
          padding: "4px 12px", fontSize: 12, cursor: scanning ? "not-allowed" : "pointer",
          opacity: scanning ? 0.7 : 1,
        }}
      >
        {scanning ? "Scansione..." : "Scansiona"}
      </button>
      {!depsOk && (
        <span style={{ fontSize: 11, color: "#f97316", marginLeft: 4 }}>
          Qdrant/embedder non disponibile — la scansione sara' limitata ai controlli statici
        </span>
      )}
      {highCount > 0 && fixQueue.length === 0 && (
        <>
          <button
            onClick={() => startFixQueue(allActiveFindings.filter(f => f.severity === "high"))}
            disabled={!onSendToChat}
            title="Invia i problemi HIGH un file alla volta — clicca 'File successivo' dopo ogni correzione"
            style={{
              background: "#ef4444", color: "#fff", border: "none", borderRadius: 6,
              padding: "4px 12px", fontSize: 12, cursor: "pointer",
            }}
          >
            Fix Tutto (High: {highCount})
          </button>
          <button
            onClick={() => startFixQueue(allActiveFindings.filter(f => f.severity === "high"), true)}
            disabled={!onSendToChat}
            title="Invia tutti i file HIGH in sequenza automatica — l'agente li corregge uno dopo l'altro senza intervento"
            style={{
              background: "#7c3aed", color: "#fff", border: "none", borderRadius: 6,
              padding: "4px 12px", fontSize: 12, cursor: "pointer",
            }}
          >
            Auto Fix
          </button>
        </>
      )}
      {fixQueue.length > 0 && fixQueueIndex < fixQueue.length && (
        <>
          {!autoFixEnabled && (
            <>
              <button
                onClick={handleFixNext}
                disabled={!onSendToChat}
                title={`Prossimo file: ${fixQueue[fixQueueIndex]?.filePath}`}
                style={{
                  background: "#f97316", color: "#fff", border: "none", borderRadius: 6,
                  padding: "4px 12px", fontSize: 12, cursor: "pointer", display: "flex", alignItems: "center", gap: 5,
                }}
              >
                ▶ File successivo ({fixQueueIndex}/{fixQueue.length})
              </button>
              <button
                onClick={() => { setAutoFixEnabled(true); handleFixNext(); }}
                disabled={!onSendToChat}
                title="Continua automaticamente tutti i file rimanenti nella coda"
                style={{
                  background: "#7c3aed", color: "#fff", border: "none", borderRadius: 6,
                  padding: "4px 12px", fontSize: 12, cursor: "pointer",
                }}
              >
                Riprendi Auto
              </button>
            </>
          )}
          {autoFixEnabled && (
            <span style={{ fontSize: 11, color: "#7c3aed", fontWeight: 600, display: "flex", alignItems: "center", gap: 4 }}>
              Auto Fix {fixQueueIndex}/{fixQueue.length}
              <button
                onClick={() => setAutoFixEnabled(false)}
                style={{
                  background: "transparent", color: "#ef4444", border: `1px solid #ef4444`,
                  borderRadius: 4, padding: "1px 6px", fontSize: 10, cursor: "pointer", marginLeft: 4,
                }}
                title="Ferma auto-fix"
              >
                Stop
              </button>
            </span>
          )}
        </>
      )}
      {fixQueue.length > 0 && fixQueueIndex >= fixQueue.length && (
        <span style={{ fontSize: 11, color: "#22c55e", fontWeight: 600 }}>
          ✓ Tutti i {fixQueue.length} file inviati
        </span>
      )}
      {fixQueue.length > 0 && (
        <button
          onClick={() => {
            setFixQueue([]);
            setFixQueueIndex(0);
            setAutoFixEnabled(false);
            // Pulisce subito sessionStorage per evitare che un remount ricarichi la vecchia coda
            try {
              const s = sessionStorage.getItem(storageKey);
              const data = s ? JSON.parse(s) : {};
              sessionStorage.setItem(storageKey, JSON.stringify({ ...data, fixQueue: [], fixQueueIndex: 0 }));
            } catch { /* ignore */ }
          }}
          title="Azzera la coda e ricomincia"
          style={{
            background: "transparent", color: tc.textMuted, border: `1px solid ${tc.border}`,
            borderRadius: 6, padding: "2px 8px", fontSize: 11, cursor: "pointer",
          }}
        >
          ✕ Reset coda
        </button>
      )}
      {scanResult && (
        <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 4 }}>
          {allActiveFindings.length} attivi su {scanResult.filesScanned} file
          {scanResult.totalFindings !== allActiveFindings.length && (
            <span title={`La scansione ha trovato ${scanResult.totalFindings} problemi totali; ${scanResult.totalFindings - allActiveFindings.length} sono falsi positivi o già risolti`}>
              {" "}({scanResult.totalFindings} scan)
            </span>
          )}
        </span>
      )}
      {/* AI Deep Review — inline nella toolbar */}
      <button
        onClick={handleDeepReview}
        disabled={deepReviewSubmitting || deepReviewState === "JOB_STATE_RUNNING" || deepReviewState === "JOB_STATE_PENDING"}
        title="Analisi approfondita AI su tutti i file sorgente. Elaborazione in background."
        style={{
          background: "#7c3aed", color: "#fff", border: "none", borderRadius: 6,
          padding: "4px 10px", fontSize: 12,
          cursor: (deepReviewSubmitting || deepReviewState === "JOB_STATE_RUNNING") ? "not-allowed" : "pointer",
          opacity: (deepReviewSubmitting || deepReviewState === "JOB_STATE_RUNNING") ? 0.7 : 1,
        }}
      >
        {deepReviewSubmitting ? "⏳" : "🔬 AI"}
      </button>
      {(deepReviewState === "JOB_STATE_RUNNING" || deepReviewState === "JOB_STATE_PENDING") && (
        <>
          <span style={{ fontSize: 11, color: "#7c3aed" }}>
            {deepReviewCompleted}/{deepReviewTotal}…
          </span>
          {/* Pulsante reset per job bloccati (es. dopo riavvio backend) */}
          <button
            onClick={() => {
              stopDeepReviewPoll();
              setDeepReviewJobId(null);
              setDeepReviewState(null);
              setDeepReviewError(null);
              setDeepReviewCompleted(0);
              setDeepReviewTotal(0);
            }}
            title="Annulla / Resetta analisi bloccata"
            style={{
              background: "transparent", color: "#ef4444", border: `1px solid #ef444444`,
              borderRadius: 5, padding: "1px 5px", fontSize: 10, cursor: "pointer",
            }}
          >✕</button>
        </>
      )}
      {deepReviewState === "JOB_STATE_SUCCEEDED" && (
        <span style={{ fontSize: 11, color: "#22c55e" }} title="Analisi AI completata">✓ AI</span>
      )}
      {deepReviewError && (
        <span style={{ fontSize: 11, color: "#ef4444", cursor: "pointer" }}
          title={`${deepReviewError} — clicca per resettare`}
          onClick={() => { setDeepReviewError(null); setDeepReviewJobId(null); setDeepReviewState(null); }}
        >⚠ AI ✕</span>
      )}
      {deepReviewJobId && deepReviewState !== "JOB_STATE_SUCCEEDED" && !deepReviewError &&
       deepReviewState !== "JOB_STATE_RUNNING" && deepReviewState !== "JOB_STATE_PENDING" && (
        <button
          onClick={() => pollDeepReviewStatus(deepReviewJobId)}
          style={{
            background: "transparent", color: tc.textMuted, border: `1px solid ${tc.border}`,
            borderRadius: 6, padding: "2px 6px", fontSize: 10, cursor: "pointer",
          }}
          title="Aggiorna stato analisi AI"
        >↻</button>
      )}
      {scanResult && (
        <div style={{ display: "flex", gap: 8, marginLeft: "auto" }}>
          {highCount > 0 && <span style={{ fontSize: 11, color: "#ef4444", fontWeight: 600 }}>• {highCount} HIGH</span>}
          {mediumCount > 0 && <span style={{ fontSize: 11, color: "#f97316", fontWeight: 600 }}>• {mediumCount} MED</span>}
          {lowCount > 0 && <span className="text-xs text-muted">• {lowCount} LOW</span>}
        </div>
      )}
    </div>
  );
}
