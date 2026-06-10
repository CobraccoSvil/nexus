"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../lib/theme";
import {
  refreshDispatcher,
  selectConnection,
  useProjectStore,
} from "../lib/project-dispatcher";

/**
 * Badge che mostra lo stato della connessione al dispatcher centrale.
 * Da posizionare nell'header dell'IDE. Quando rosso (`disconnected`)
 * l'utente sa che i dati nei pannelli sono potenzialmente stantii.
 */
export function ConnectionStatusBadge() {
  const tc = useThemeColors();
  const status = useProjectStore(selectConnection);

  const color = (() => {
    switch (status) {
      case "open": return "#10b981";
      case "connecting": return "#fbbf24";
      case "reconnecting": return "#f97316";
      case "disconnected": return "#ef4444";
      default: return tc.textMuted;
    }
  })();

  const label = (() => {
    switch (status) {
      case "open": return "live";
      case "connecting": return "connessione...";
      case "reconnecting": return "riconnessione...";
      case "disconnected": return "disconnesso";
      default: return "inattivo";
    }
  })();

  const canRetry = status === "disconnected";

  return (
    <button
      type="button"
      onClick={canRetry ? () => refreshDispatcher() : undefined}
      disabled={!canRetry}
      title={canRetry ? "Clicca per riprovare la connessione" : `Dispatcher: ${label}`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        background: "transparent",
        border: `1px solid ${tc.border}`,
        borderRadius: 12,
        padding: "2px 8px",
        fontSize: 10,
        color: tc.textMuted,
        cursor: canRetry ? "pointer" : "default",
      }}
    >
      <span
        style={{
          width: 8,
          height: 8,
          borderRadius: "50%",
          background: color,
          boxShadow: status === "open" ? `0 0 6px ${color}` : "none",
        }}
      />
      {label}
    </button>
  );
}

/**
 * Hook helper: applica un'animazione di flash a un elemento quando il pannello
 * specificato e' marked `highlight_panel` da un evento. Usato dai tab del
 * bottom panel.
 */
export function usePanelHighlight(panel: string): boolean {
  const expires = useProjectStore((s) => s.panelHighlights[panel]);
  const [active, setActive] = useState(false);

  useEffect(() => {
    if (!expires) return;
    const remaining = expires - Date.now();
    if (remaining <= 0) return;
    setActive(true);
    const t = window.setTimeout(() => setActive(false), remaining);
    return () => window.clearTimeout(t);
  }, [expires]);

  return active;
}
