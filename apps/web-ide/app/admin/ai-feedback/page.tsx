"use client";

import { useEffect, useState } from "react";
import {
  getAdminFeedbackErrors,
  reviewAdminFeedbackError,
  type AdminFeedbackItem,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { useGlobalDialog } from "../../../components/global-dialog-provider";

export default function AdminAiFeedbackPage() {
  const tc = useThemeColors();
  const { promptDialog } = useGlobalDialog();
  const [feedback, setFeedback] = useState<AdminFeedbackItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadFeedback = async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await getAdminFeedbackErrors();
      setFeedback(response.feedback ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Impossibile caricare i feedback.");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadFeedback();
  }, []);

  const review = async (
    item: AdminFeedbackItem,
    status: "reviewed" | "resolved" | "rejected",
  ) => {
    setBusyId(item.id);
    setError(null);
    try {
      const note =
        (await promptDialog(
          "Nota review (opzionale):",
          item.reviewNote ?? "",
          "Review feedback",
        )) ?? undefined;
      await reviewAdminFeedbackError(item.id, status, note);
      await loadFeedback();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Aggiornamento feedback fallito.");
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div>
        <h1 style={{ fontSize: 20, fontWeight: 600, marginBottom: 6 }}>AI Feedback</h1>
        <p style={{ color: tc.textMuted, fontSize: 13, margin: 0 }}>
          Review dei feedback errore inviati dalla chat e controllo qualità auto-learning.
        </p>
      </div>

      {error && (
        <div
          style={{
            padding: "10px 14px",
            borderRadius: 8,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            background: tc.bgCard,
            fontSize: 13,
          }}
        >
          {error}
        </div>
      )}

      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          gap: 12,
          padding: 14,
          borderRadius: 10,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
        }}
      >
        <div style={{ fontSize: 13 }}>Feedback trovati: {feedback.length}</div>
        <button onClick={() => void loadFeedback()} style={buttonStyle(tc)} disabled={loading}>
          {loading ? "Aggiorno..." : "Aggiorna"}
        </button>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {feedback.map((item) => (
          <div
            key={item.id}
            style={{
              padding: 14,
              borderRadius: 10,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              display: "flex",
              flexDirection: "column",
              gap: 10,
            }}
          >
            {/* Header row */}
            <div style={{ display: "flex", justifyContent: "space-between", gap: 8, flexWrap: "wrap" }}>
              <div style={{ fontSize: 12, color: tc.textMuted }}>
                {item.createdAt} • {item.provider ?? "-"} / {item.model ?? "-"} • intent:{" "}
                {item.intent ?? "chat"}
                {(item as AdminFeedbackItem & { retrievedCount?: number }).retrievedCount != null && (item as AdminFeedbackItem & { retrievedCount?: number }).retrievedCount! > 0 && (
                  <span style={{ marginLeft: 8, color: tc.success }}>
                    ↺ usata {(item as AdminFeedbackItem & { retrievedCount?: number }).retrievedCount}×
                  </span>
                )}
              </div>
              <div style={{ fontSize: 12, color: tc.accent }}>status: {item.status}</div>
            </div>

            {/* Domanda utente che ha generato la risposta sbagliata */}
            {(item as AdminFeedbackItem & { userQuestionPreview?: string }).userQuestionPreview && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, textTransform: "uppercase", letterSpacing: "0.05em" }}>
                  Domanda utente
                </div>
                <div
                  style={{
                    fontSize: 12,
                    color: tc.text,
                    background: tc.bg,
                    border: `1px solid ${tc.border}`,
                    borderRadius: 6,
                    padding: "6px 8px",
                    whiteSpace: "pre-wrap",
                    fontStyle: "italic",
                  }}
                >
                  {(item as AdminFeedbackItem & { userQuestionPreview?: string }).userQuestionPreview}
                </div>
              </div>
            )}

            {/* Preview risposta AI sbagliata */}
            {(item as AdminFeedbackItem & { aiResponsePreview?: string }).aiResponsePreview && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, textTransform: "uppercase", letterSpacing: "0.05em" }}>
                  Risposta AI (errata)
                </div>
                <div
                  style={{
                    fontSize: 12,
                    color: tc.error,
                    background: tc.bg,
                    border: `1px solid ${tc.error}44`,
                    borderRadius: 6,
                    padding: "6px 8px",
                    whiteSpace: "pre-wrap",
                    maxHeight: 120,
                    overflow: "auto",
                  }}
                >
                  {(item as AdminFeedbackItem & { aiResponsePreview?: string }).aiResponsePreview}
                </div>
              </div>
            )}

            {/* Commento utente */}
            <div>
              <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, textTransform: "uppercase", letterSpacing: "0.05em" }}>
                Feedback utente
              </div>
              <div style={{ fontSize: 13, whiteSpace: "pre-wrap", color: tc.text }}>{item.comment}</div>
            </div>

            {/* Correzione che verrà iniettata nel prompt */}
            {(item as AdminFeedbackItem & { correctionText?: string }).correctionText && (item as AdminFeedbackItem & { correctionText?: string }).correctionText !== item.comment && (
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 3, textTransform: "uppercase", letterSpacing: "0.05em" }}>
                  Correzione iniettata nel prompt
                </div>
                <div
                  style={{
                    fontSize: 12,
                    color: tc.success,
                    background: tc.bg,
                    border: `1px solid ${tc.success}44`,
                    borderRadius: 6,
                    padding: "6px 8px",
                    whiteSpace: "pre-wrap",
                    fontFamily: '"JetBrains Mono", monospace',
                  }}
                >
                  {(item as AdminFeedbackItem & { correctionText?: string }).correctionText}
                </div>
              </div>
            )}

            {item.reviewNote && (
              <div style={{ fontSize: 12, color: tc.textMuted }}>📝 Nota review: {item.reviewNote}</div>
            )}

            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button
                disabled={busyId === item.id}
                onClick={() => void review(item, "reviewed")}
                style={buttonStyle(tc)}
              >
                Review
              </button>
              <button
                disabled={busyId === item.id}
                onClick={() => void review(item, "resolved")}
                style={buttonStyle(tc)}
              >
                Risolto
              </button>
              <button
                disabled={busyId === item.id}
                onClick={() => void review(item, "rejected")}
                style={buttonStyle(tc)}
              >
                Rifiuta
              </button>
            </div>
          </div>
        ))}
        {!loading && feedback.length === 0 && (
          <div
            style={{
              padding: 14,
              borderRadius: 10,
              border: `1px solid ${tc.border}`,
              color: tc.textMuted,
              background: tc.bgCard,
              fontSize: 13,
            }}
          >
            Nessun feedback disponibile.
          </div>
        )}
      </div>
    </div>
  );
}

function buttonStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    padding: "7px 11px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "inherit",
  } as const;
}
