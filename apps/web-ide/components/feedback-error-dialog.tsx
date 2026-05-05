"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../lib/theme";
import { feedbackAssist } from "../lib/api-client";

interface FeedbackErrorDialogProps {
  /** Contenuto della risposta AI problematica */
  messageContent: string;
  onConfirm: (description: string) => void;
  onCancel: () => void;
}

export function FeedbackErrorDialog({
  messageContent,
  onConfirm,
  onCancel,
}: FeedbackErrorDialogProps) {
  const tc = useThemeColors();
  const [description, setDescription] = useState("");
  const [aiLoading, setAiLoading] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  const handleAiAssist = useCallback(async () => {
    setAiLoading(true);
    setAiError(null);
    try {
      const res = await feedbackAssist(messageContent, description);
      if (res.suggestion) {
        setDescription(res.suggestion);
        // Sposta il cursore alla fine
        setTimeout(() => {
          const ta = textareaRef.current;
          if (ta) {
            ta.focus();
            ta.selectionStart = ta.selectionEnd = ta.value.length;
          }
        }, 50);
      }
    } catch {
      setAiError("Assistente AI non disponibile al momento.");
    } finally {
      setAiLoading(false);
    }
  }, [messageContent, description]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Escape") onCancel();
      // Ctrl+Enter per confermare
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        if (description.trim()) onConfirm(description.trim());
      }
    },
    [description, onConfirm, onCancel],
  );

  // Anteprima del contenuto AI (prime 3 righe o max 200 char)
  const contentPreview = messageContent
    .split("\n")
    .slice(0, 3)
    .join(" ")
    .slice(0, 200)
    .trim();
  const hasPreview = contentPreview.length > 0;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(5, 10, 18, 0.5)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1200,
        padding: 16,
      }}
      onClick={onCancel}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Segnala errore"
        style={{
          width: 520,
          maxWidth: "95vw",
          borderRadius: 10,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          boxShadow: "0 14px 44px rgba(0,0,0,0.35)",
          padding: 16,
          display: "flex",
          flexDirection: "column",
          gap: 12,
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Titolo */}
        <div style={{ color: tc.text, fontWeight: 700, fontSize: 14 }}>
          Segnala errore
        </div>

        {/* Anteprima risposta AI */}
        {hasPreview && (
          <div
            style={{
              fontSize: 11,
              color: tc.textSecondary,
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 6,
              padding: "6px 8px",
              fontStyle: "italic",
              lineHeight: 1.4,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
            title={messageContent}
          >
            Risposta AI: {contentPreview}
            {messageContent.length > 200 ? "…" : ""}
          </div>
        )}

        {/* Label */}
        <div style={{ color: tc.textSecondary, fontSize: 13 }}>
          Descrivi l&apos;errore della risposta AI:
        </div>

        {/* Textarea ridimensionabile */}
        <textarea
          ref={textareaRef}
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Es: La risposta ha identificato erroneamente un pattern JSX come N+1 query…"
          rows={4}
          style={{
            width: "100%",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: tc.text,
            padding: "8px 10px",
            fontSize: 13,
            boxSizing: "border-box",
            resize: "vertical",
            minHeight: 90,
            maxHeight: 300,
            lineHeight: 1.5,
            fontFamily: "inherit",
            outline: "none",
            transition: "border-color 0.15s",
          }}
          onFocus={(e) => {
            e.currentTarget.style.borderColor = tc.accent;
          }}
          onBlur={(e) => {
            e.currentTarget.style.borderColor = tc.border;
          }}
        />

        {/* AI Error */}
        {aiError && (
          <div style={{ color: "#e05", fontSize: 12 }}>{aiError}</div>
        )}

        {/* Footer */}
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 8,
          }}
        >
          {/* AI Assist button */}
          <button
            type="button"
            disabled={aiLoading}
            onClick={handleAiAssist}
            title="Lascia che l'AI analizzi la risposta e suggerisca una descrizione"
            style={{
              display: "flex",
              alignItems: "center",
              gap: 5,
              border: `1px solid ${tc.accent}`,
              background: "transparent",
              color: tc.accent,
              borderRadius: 7,
              padding: "5px 10px",
              fontSize: 12,
              cursor: aiLoading ? "wait" : "pointer",
              fontFamily: "inherit",
              opacity: aiLoading ? 0.7 : 1,
              transition: "opacity 0.15s",
            }}
          >
            <span style={{ fontSize: 14 }}>✦</span>
            {aiLoading ? "Analisi in corso…" : "Aiuta con AI"}
          </button>

          {/* Annulla + OK */}
          <div style={{ display: "flex", gap: 8 }}>
            <button
              type="button"
              onClick={onCancel}
              style={btnStyle(tc, false)}
            >
              Annulla
            </button>
            <button
              type="button"
              disabled={!description.trim()}
              onClick={() => {
                if (description.trim()) onConfirm(description.trim());
              }}
              style={{
                ...btnStyle(tc, true),
                opacity: description.trim() ? 1 : 0.45,
                cursor: description.trim() ? "pointer" : "default",
              }}
            >
              OK
            </button>
          </div>
        </div>

        {/* Hint tastiera */}
        <div
          style={{
            fontSize: 10,
            color: tc.textSecondary,
            opacity: 0.6,
            textAlign: "right",
          }}
        >
          Ctrl+Enter per confermare · Esc per annullare
        </div>
      </div>
    </div>
  );
}

function btnStyle(tc: ReturnType<typeof useThemeColors>, primary: boolean) {
  return {
    border: `1px solid ${primary ? tc.accent : tc.border}`,
    background: primary ? tc.accentBg : tc.bgInput,
    color: primary ? tc.accent : tc.text,
    borderRadius: 8,
    padding: "6px 14px",
    fontSize: 12,
    cursor: "pointer",
    fontFamily: "inherit",
  } as const;
}
