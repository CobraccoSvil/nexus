"use client";

import type { PrecheckResult } from "../../lib/api-client";

/**
 * Widget di suggerimento precheck: mostra il testo corretto, l'eventuale
 * suggerimento di contesto e i problemi rilevati. Le azioni inviano
 * direttamente (via `onSend`) il testo corretto / con contesto / originale.
 *
 * Nota: il precheck e' attualmente disabilitato (`precheckPending` resta
 * false e `precheckResult` resta null in chat-panel), ma il componente e'
 * conservato perche' la UI puo' essere riattivata senza re-implementazione.
 */
export function PrecheckSuggestion({
  precheckPending,
  precheckResult,
  onClose,
  onSend,
  tc,
}: {
  precheckPending: boolean;
  precheckResult: (PrecheckResult & { originalText: string }) | null;
  onClose: () => void;
  onSend: (text: string) => void;
  tc: Record<string, string>;
}) {
  return (
    <>
      {precheckPending && (
        <div style={{
          margin: "0 8px 4px",
          padding: "8px 12px",
          borderRadius: 8,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          fontSize: 12,
          color: tc.textMuted,
          display: "flex",
          alignItems: "center",
          gap: 8,
        }}>
          <span style={{ animation: "spin 1s linear infinite", display: "inline-block" }}>⟳</span>
          Controllo ortografia e contesto…
        </div>
      )}
      {precheckResult && !precheckPending && (
        <div style={{
          margin: "0 8px 6px",
          borderRadius: 8,
          border: `1px solid ${tc.accent}66`,
          background: tc.bgCard,
          fontSize: 12,
          overflow: "hidden",
        }}>
          {/* Header */}
          <div style={{
            padding: "7px 12px",
            borderBottom: `1px solid ${tc.border}`,
            background: tc.bg,
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}>
            <span style={{ fontWeight: 600, color: tc.accent, fontSize: 11 }}>
              ✦ Suggerimento
            </span>
            <button
              onClick={onClose}
              style={{ background: "none", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 14, lineHeight: 1 }}
            >×</button>
          </div>

          <div style={{ padding: "10px 12px", display: "flex", flexDirection: "column", gap: 8 }}>
            {/* Testo corretto */}
            {precheckResult.correctedText && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.04em" }}>
                  Testo corretto
                </div>
                <div style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: tc.bg,
                  border: `1px solid ${tc.success}44`,
                  color: tc.text,
                  whiteSpace: "pre-wrap",
                }}>
                  {precheckResult.correctedText}
                </div>
              </div>
            )}

            {/* Suggerimento contesto */}
            {precheckResult.contextSuggestion && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4, textTransform: "uppercase", letterSpacing: "0.04em" }}>
                  Aggiungi contesto
                </div>
                <div style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: tc.bg,
                  border: `1px solid ${tc.accent}44`,
                  color: tc.textMuted,
                  fontStyle: "italic",
                }}>
                  {precheckResult.contextSuggestion}
                </div>
              </div>
            )}

            {/* Problemi */}
            {(precheckResult.issues?.length ?? 0) > 0 && (
              <div style={{ fontSize: 11, color: tc.textMuted }}>
                {(precheckResult.issues ?? []).map((issue, i) => (
                  <span key={i} style={{ marginRight: 8 }}>• {issue}</span>
                ))}
              </div>
            )}

            {/* Azioni */}
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 2 }}>
              {precheckResult.correctedText && (
                <button
                  onClick={() => {
                    // Invia direttamente il testo corretto (non ri-triggerare il precheck)
                    onSend(precheckResult.correctedText!);
                  }}
                  style={{
                    padding: "5px 12px", borderRadius: 6, border: "none",
                    background: tc.accent, color: "#fff",
                    cursor: "pointer", fontSize: 11, fontWeight: 600,
                  }}
                >
                  Usa testo corretto
                </button>
              )}
              {precheckResult.contextSuggestion && (
                <button
                  onClick={() => {
                    // Invia direttamente il testo originale + suggerimento contesto
                    onSend(precheckResult.originalText + "\n\n" + precheckResult.contextSuggestion!);
                  }}
                  style={{
                    padding: "5px 12px", borderRadius: 6,
                    border: `1px solid ${tc.accent}`,
                    background: "none", color: tc.accent,
                    cursor: "pointer", fontSize: 11,
                  }}
                >
                  Aggiungi contesto
                </button>
              )}
              <button
                onClick={() => onSend(precheckResult.originalText)}
                style={{
                  padding: "5px 12px", borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: "none", color: tc.textMuted,
                  cursor: "pointer", fontSize: 11,
                }}
              >
                Invia comunque
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
