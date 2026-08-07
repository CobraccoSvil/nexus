"use client";

import { useState } from "react";
import type { SavedChatAttachment } from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { ModalPortal } from "../modal-portal";
import { useI18n } from "../../lib/i18n";

/* ------------------------------------------------------------------ */
/* AttachmentIndexDialog                                              */
/* ------------------------------------------------------------------ */
/**
 * Dialog modale per la scelta dell'utente di quali allegati indicizzare
 * nella Knowledge Base. Pre-spunta i 'text' (gia' indicizzabili come body)
 * e lascia non spuntati image/binary. L'utente puo' scegliere un
 * sottoinsieme o saltare tutto. La chiamata vera all'API la fa il chiamante.
 */
export function AttachmentIndexDialog({
  proposal,
  onClose,
  onConfirm,
  tc,
}: {
  proposal: { messageId: string; attachments: SavedChatAttachment[] };
  onClose: () => void;
  onConfirm: (attachmentIds: string[]) => void | Promise<void>;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const { t } = useI18n();
  // Stato di selezione: default = solo i 'text' pre-spuntati.
  // Gli 'binary' restano disabilitati (backend rifiuta comunque).
  const initialSelected = new Set<string>(
    proposal.attachments
      .filter((att) => att.kind === "text")
      .map((att) => att.id),
  );
  const [selected, setSelected] = useState<Set<string>>(initialSelected);
  const [submitting, setSubmitting] = useState(false);

  const toggle = (id: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const handleConfirm = async () => {
    if (submitting) return;
    setSubmitting(true);
    try {
      await onConfirm(Array.from(selected));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <ModalPortal>
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t("chat.indicizzazioneAllegatiNellaKnowledge")}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(5, 10, 18, 0.5)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1300,
        padding: 16,
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget && !submitting) onClose();
      }}
    >
      <div
        style={{
          width: 520,
          maxWidth: "95vw",
          maxHeight: "85vh",
          overflow: "auto",
          borderRadius: 10,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          boxShadow: "0 14px 44px rgba(0,0,0,0.4)",
          padding: 16,
          display: "flex",
          flexDirection: "column",
          gap: 12,
        }}
      >
        <div style={{ color: tc.text, fontWeight: 700, fontSize: 15 }}>
          {t("chat.indicizzareNellaKnowledgeBase")}
        </div>
        <div style={{ color: tc.textSecondary, fontSize: 12 }}>
          Gli allegati salvati possono essere aggiunti alla KB del progetto come
          note ricercabili. Seleziona quali file vuoi indicizzare. I file di
          testo sono pre-selezionati; immagini e binari sono disponibili come
          metadata-only o esclusi.
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {proposal.attachments.map((att) => {
            const isBinary = att.kind === "binary";
            return (
              <label
                key={att.id}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "6px 8px",
                  borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: isBinary ? tc.bgInput : tc.bgCard,
                  color: isBinary ? tc.textMuted : tc.text,
                  cursor: isBinary ? "not-allowed" : "pointer",
                  fontSize: 12,
                }}
                title={
                  isBinary
                    ? "Tipo binario non indicizzabile nella KB"
                    : att.fileName
                }
              >
                <input
                  type="checkbox"
                  disabled={isBinary || submitting}
                  checked={!isBinary && selected.has(att.id)}
                  onChange={() => !isBinary && toggle(att.id)}
                />
                <span
                  aria-hidden
                  style={{
                    fontSize: 10,
                    fontWeight: 700,
                    color: tc.textSecondary,
                    letterSpacing: "0.5px",
                    minWidth: 28,
                    textAlign: "center",
                  }}
                >
                  {att.kind === "image"
                    ? "IMG"
                    : att.kind === "text"
                      ? "TXT"
                      : "BIN"}
                </span>
                <span
                  style={{
                    flex: 1,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {att.fileName}
                </span>
                <span style={{ color: tc.textMuted, fontSize: 11 }}>
                  {att.kind}
                </span>
              </label>
            );
          })}
        </div>
        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            marginTop: 4,
          }}
        >
          <button
            type="button"
            disabled={submitting}
            onClick={onClose}
            style={{
              border: `1px solid ${tc.border}`,
              background: tc.bgInput,
              color: tc.text,
              borderRadius: 8,
              padding: "6px 12px",
              fontSize: 12,
              cursor: submitting ? "not-allowed" : "pointer",
              fontFamily: "inherit",
            }}
          >
            {t("chat.saltaTutto")}
          </button>
          <button
            type="button"
            disabled={submitting || selected.size === 0}
            onClick={() => void handleConfirm()}
            style={{
              border: `1px solid ${tc.accent}`,
              background: tc.accentBg,
              color: tc.accent,
              borderRadius: 8,
              padding: "6px 12px",
              fontSize: 12,
              cursor:
                submitting || selected.size === 0 ? "not-allowed" : "pointer",
              fontWeight: 700,
              fontFamily: "inherit",
              opacity: selected.size === 0 ? 0.5 : 1,
            }}
          >
            {submitting
              ? "Indicizzazione…"
              : `Indicizza selezionati (${selected.size})`}
          </button>
        </div>
      </div>
    </div>
    </ModalPortal>
  );
}
