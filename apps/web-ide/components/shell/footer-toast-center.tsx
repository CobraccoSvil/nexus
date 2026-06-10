"use client";

import type { CSSProperties } from "react";
import type { useThemeColors } from "../../lib/theme";
import { selectToasts, useProjectStore } from "../../lib/project-dispatcher";
import { useActionFeedback } from "../global-action-feedback-provider";

type ThemeColors = ReturnType<typeof useThemeColors>;

/**
 * Messaggio di stato centrato nel footer dell'IDE. Punto unico di
 * visualizzazione dei toast informativi: rimpiazza i popup invasivi che prima
 * comparivano in alto a destra (badge "operazione in corso") e in basso a
 * destra (stack toast SSE + esito azioni). Mostra una sola riga, troncata,
 * in ordine di priorita':
 *   1) operazione fetch in corso (stato live, con dot pulsante);
 *   2) l'ultimo toast attivo dallo store (eventi SSE/ui_hint + pushToast
 *      programmatico + esiti delle mutazioni intercettate dal feedback globale).
 */
export function FooterToastCenter({ tc }: { tc: ThemeColors }) {
  const toasts = useProjectStore(selectToasts);
  const dismiss = useProjectStore((s) => s.dismissToast);
  const { pendingCount, pendingLabel } = useActionFeedback();

  // 1) Operazione in corso: stato live, priorita' massima.
  if (pendingCount > 0) {
    const text =
      pendingCount > 1
        ? `${pendingCount} operazioni in corso`
        : `${pendingLabel} in corso`;
    return (
      <div style={rowStyle} aria-live="polite" title={text}>
        <span
          style={{
            width: 7,
            height: 7,
            borderRadius: "50%",
            background: tc.accent,
            flexShrink: 0,
            animation: "pulse 1s ease-in-out infinite",
          }}
        />
        <span style={messageStyle(tc.textSecondary)}>{text}</span>
      </div>
    );
  }

  // 2) Ultimo toast informativo dallo store.
  const last = toasts.length > 0 ? toasts[toasts.length - 1] : null;
  if (!last) return null;
  const color = colorForSeverity(last.severity, tc);

  return (
    <div style={rowStyle} aria-live="polite" title={last.message}>
      <span
        style={{
          width: 7,
          height: 7,
          borderRadius: "50%",
          background: color,
          flexShrink: 0,
        }}
      />
      <span style={messageStyle(color)}>{last.message}</span>
      <button
        type="button"
        onClick={() => dismiss(last.id)}
        aria-label="Chiudi notifica"
        style={dismissBtnStyle}
      >
        ×
      </button>
    </div>
  );
}

function colorForSeverity(severity: string, tc: ThemeColors): string {
  switch (severity) {
    case "success":
      return "#22c55e";
    case "warning":
      return "#d97706";
    case "error":
      return tc.error;
    default:
      return tc.accent;
  }
}

const rowStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: 6,
  maxWidth: 560,
  minWidth: 0,
};

function messageStyle(color: string): CSSProperties {
  return {
    color,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  };
}

const dismissBtnStyle: CSSProperties = {
  background: "transparent",
  border: "none",
  color: "inherit",
  cursor: "pointer",
  fontSize: 13,
  lineHeight: 1,
  padding: 0,
  opacity: 0.7,
  flexShrink: 0,
};
