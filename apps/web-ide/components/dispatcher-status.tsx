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
export function ConnectionStatusBadge({ compact = false }: { compact?: boolean } = {}) {
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
  // In header stretto (compact) mostriamo solo il pallino: lo stato resta nel
  // title (hover) e il colore comunica gia' disconnesso/live. Coerente con i
  // pallini provider che a loro volta perdono l'etichetta a viewport narrow.
  const title = canRetry
    ? `Dispatcher: ${label} - clicca per riconnettere`
    : `Dispatcher: ${label}`;

  return (
    <button
      type="button"
      onClick={canRetry ? () => refreshDispatcher() : undefined}
      disabled={!canRetry}
      title={title}
      aria-label={title}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: compact ? 0 : 6,
        background: "transparent",
        border: `1px solid ${tc.border}`,
        borderRadius: 12,
        padding: compact ? 3 : "2px 8px",
        fontSize: 10,
        lineHeight: 1,
        color: tc.textMuted,
        cursor: canRetry ? "pointer" : "default",
        flexShrink: 0,
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
      {!compact && label}
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
