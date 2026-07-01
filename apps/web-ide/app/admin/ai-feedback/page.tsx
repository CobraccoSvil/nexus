"use client";

import { useEffect, useState, type CSSProperties } from "react";
import {
  getAdminFeedbackErrors,
  reviewAdminFeedbackError,
  type AdminFeedbackItem,
} from "../../../lib/api-client";
import { useThemeColors, type Theme } from "../../../lib/theme";
import { useGlobalDialog } from "../../../components/global-dialog-provider";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";

/** Blocco "label + preview boxed". Punto unico per i 3 preview ripetuti
 *  (Domanda utente, Risposta AI errata, Feedback utente). Prima i 3 blocchi
 *  con stessa struttura `<div><div>LABEL</div><div STYLED>CONTENT</div></div>`
 *  causavano un clone 50L intra-file. */
function LabeledPreview({
  tc,
  label,
  content,
  boxStyle,
}: {
  tc: Theme;
  label: string;
  content: string;
  boxStyle?: CSSProperties;
}) {
  return (
    <div>
      <div
        style={{
          fontSize: 11,
          color: tc.textMuted,
          marginBottom: 3,
          textTransform: "uppercase",
          letterSpacing: "0.05em",
        }}
      >
        {label}
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
          ...boxStyle,
        }}
      >
        {content}
      </div>
    </div>
  );
}

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
      <AdminPageHeader
        title="AI Feedback"
        description="Review dei feedback errore inviati dalla chat e controllo qualità auto-learning."
      />


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
              <LabeledPreview
                tc={tc}
                label="Domanda utente"
                content={(item as AdminFeedbackItem & { userQuestionPreview?: string }).userQuestionPreview!}
                boxStyle={{ fontStyle: "italic" }}
              />
            )}

            {/* Preview risposta AI sbagliata */}
            {(item as AdminFeedbackItem & { aiResponsePreview?: string }).aiResponsePreview && (
              <LabeledPreview
                tc={tc}
                label="Risposta AI (errata)"
                content={(item as AdminFeedbackItem & { aiResponsePreview?: string }).aiResponsePreview!}
                boxStyle={{
                  color: tc.error,
                  border: `1px solid ${tc.error}44`,
                  maxHeight: 120,
                  overflow: "auto",
                }}
              />
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
                    fontFamily: 'var(--font-mono)',
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
