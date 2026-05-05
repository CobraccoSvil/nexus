"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { requestProjectDbOverride } from "../../lib/api-client";

interface OverrideConfirmDialogProps {
  projectId: string;
  projectName: string;
  sql: string;
  onConfirmed: () => void;
  onCancel: () => void;
}

/**
 * Dialog di conferma override DDL per un progetto utente.
 * Richiede che l'utente digiti il nome del progetto come doppia conferma,
 * allineandosi al pattern UX delle operazioni distruttive in Nexus.
 */
export function OverrideConfirmDialog({
  projectId,
  projectName,
  sql,
  onConfirmed,
  onCancel,
}: OverrideConfirmDialogProps) {
  const tc = useThemeColors();
  const [reason, setReason] = useState("");
  const [nameConfirm, setNameConfirm] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = nameConfirm === projectName && reason.trim().length >= 10;

  async function handleConfirm() {
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await requestProjectDbOverride(projectId, sql, reason);
      onConfirmed();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore durante la richiesta di override");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 200,
        background: "rgba(0,0,0,0.6)",
        display: "flex", alignItems: "center", justifyContent: "center",
        padding: 20,
      }}
    >
      <div
        style={{
          background: tc.bg,
          border: `1px solid ${tc.border}`,
          borderRadius: 12,
          padding: 28,
          maxWidth: 540,
          width: "100%",
          display: "flex",
          flexDirection: "column",
          gap: 16,
        }}
      >
        <div style={{ display: "flex", alignItems: "flex-start", gap: 12 }}>
          <div style={{
            width: 36, height: 36, borderRadius: 8,
            background: "#ef444420", border: "1px solid #ef444440",
            display: "flex", alignItems: "center", justifyContent: "center",
            flexShrink: 0, fontSize: 18,
          }}>
            ⚠
          </div>
          <div>
            <div style={{ fontSize: 16, fontWeight: 700, color: tc.text }}>
              Override DDL — Operazione non tracciabile
            </div>
            <div style={{ fontSize: 13, color: tc.textMuted, marginTop: 4 }}>
              Il guardrail migration è disabilitato per questa operazione. Il DDL verrà eseguito direttamente
              sul database del progetto e registrato come &quot;overridden&quot; nell&apos;audit trail.
            </div>
          </div>
        </div>

        {/* SQL preview */}
        <div>
          <div style={{ fontSize: 12, color: tc.textMuted, marginBottom: 6 }}>SQL che verrà eseguito:</div>
          <pre style={{
            background: tc.bgCard, border: `1px solid ${tc.border}`,
            borderRadius: 6, padding: "10px 14px",
            fontSize: 12, color: tc.text,
            overflow: "auto", maxHeight: 120,
            margin: 0, whiteSpace: "pre-wrap",
          }}>
            {sql}
          </pre>
        </div>

        {/* Reason */}
        <div>
          <label style={{ fontSize: 13, color: tc.text, fontWeight: 600 }}>
            Motivo dell&apos;override *
          </label>
          <textarea
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="Descrivi perché è necessario eseguire DDL diretto (min. 10 caratteri)..."
            rows={3}
            style={{
              width: "100%", marginTop: 6,
              background: tc.bgCard, color: tc.text,
              border: `1px solid ${reason.trim().length >= 10 ? tc.border : tc.error}`,
              borderRadius: 6, padding: "8px 12px", fontSize: 13,
              resize: "vertical", boxSizing: "border-box",
            }}
          />
        </div>

        {/* Name confirmation */}
        <div>
          <label style={{ fontSize: 13, color: tc.text, fontWeight: 600 }}>
            Digita il nome del progetto per confermare: <code style={{ color: tc.accent }}>{projectName}</code>
          </label>
          <input
            type="text"
            value={nameConfirm}
            onChange={(e) => setNameConfirm(e.target.value)}
            placeholder={projectName}
            style={{
              width: "100%", marginTop: 6,
              background: tc.bgCard, color: tc.text,
              border: `1px solid ${nameConfirm === projectName ? "#22c55e" : tc.border}`,
              borderRadius: 6, padding: "8px 12px", fontSize: 13,
              boxSizing: "border-box",
            }}
          />
        </div>

        {error && (
          <div style={{ fontSize: 13, color: tc.error }}>{error}</div>
        )}

        <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
          <button
            onClick={onCancel}
            style={{
              padding: "8px 18px", borderRadius: 8,
              border: `1px solid ${tc.border}`, background: "none",
              color: tc.text, cursor: "pointer", fontSize: 14,
            }}
          >
            Annulla
          </button>
          <button
            onClick={() => void handleConfirm()}
            disabled={!canSubmit || submitting}
            style={{
              padding: "8px 18px", borderRadius: 8, border: "none",
              background: canSubmit ? "#ef4444" : tc.bgCard,
              color: canSubmit ? "#fff" : tc.textMuted,
              cursor: canSubmit ? "pointer" : "not-allowed",
              fontSize: 14, fontWeight: 600,
            }}
          >
            {submitting ? "Invio..." : "Conferma override"}
          </button>
        </div>
      </div>
    </div>
  );
}
