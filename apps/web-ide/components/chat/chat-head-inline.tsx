"use client";

import dynamic from "next/dynamic";
import { iconButton } from "../../lib/icon-button-style";
import type { ChatHeadPopoverProps } from "./chat-head-popover";

// Stesso chunk lazy del popover: Next lo deduplica, si carica una volta sola.
const ProfileSelector = dynamic(() => import("./profile-selector.lazy"), {
  loading: () => <div style={{ fontSize: 12, opacity: 0.6 }}>Caricamento…</div>,
  ssr: false,
});

/**
 * Testata della chat distesa in riga nell'header: profilo, sessione e azioni, gli
 * stessi controlli del popover ma in orizzontale.
 *
 * Punto unico dei dati (regola L): riceve lo stesso contratto props di
 * ChatHeadPopover, cosi' la vista in riga e quella raccolta non divergono. E'
 * ChatHead a decidere quale delle due mostrare, in base alla misura.
 *
 * Gli elementi NON cedono (flexShrink: 0, nowrap): la riga espone la sua larghezza
 * NATURALE, che ChatHead misura per decidere se ci sta. Se cedessero (come faceva
 * la versione storica, dove il gruppo sessioni collassava a larghezza 0) la misura
 * mentirebbe e le chat tornerebbero irraggiungibili.
 */
export function ChatHeadInline({
  tc,
  profiles,
  selectedProfileId,
  onSelectProfile,
  onCreateProfile,
  sessions,
  activeSessionId,
  onSelectSession,
  onNewSession,
  onRenameSession,
  onDeleteSession,
  onCompactSession,
  ctxPct,
}: ChatHeadPopoverProps) {
  const coloreCtx =
    ctxPct == null ? tc.textMuted : ctxPct >= 90 ? tc.error : ctxPct >= 70 ? tc.warning : tc.textMuted;

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6, flexShrink: 0, whiteSpace: "nowrap" }}>
      <ProfileSelector
        profiles={profiles}
        selectedProfileId={selectedProfileId}
        onSelect={onSelectProfile}
        onCreateNew={onCreateProfile}
        style={{ flexShrink: 0 }}
      />
      <select
        value={activeSessionId ?? ""}
        onChange={(e) => {
          const id = e.target.value;
          if (id) onSelectSession(id);
        }}
        disabled={sessions.length === 0}
        title="Seleziona sessione chat"
        aria-label="Seleziona sessione chat"
        style={{
          borderRadius: 999,
          border: `1px solid ${tc.border}`,
          background: tc.bgInput,
          color: sessions.length === 0 ? tc.textMuted : tc.textSecondary,
          padding: "2px 8px",
          fontSize: 11,
          fontFamily: "inherit",
          minWidth: 0,
          maxWidth: 210,
          flexShrink: 0,
          cursor: sessions.length === 0 ? "not-allowed" : "pointer",
        }}
      >
        {sessions.length === 0 ? (
          <option value="">Nessuna chat</option>
        ) : (
          sessions.map((s) => (
            <option key={s.id} value={s.id}>
              {s.title}
            </option>
          ))
        )}
      </select>
      <button
        type="button"
        onClick={onNewSession}
        title="Nuova chat"
        aria-label="Nuova chat"
        style={iconButton(tc)}
      >
        ＋
      </button>
      <button
        type="button"
        disabled={!activeSessionId}
        onClick={onRenameSession}
        title="Rinomina chat"
        aria-label="Rinomina chat"
        style={iconButton(tc, !activeSessionId)}
      >
        ✎
      </button>
      <button
        type="button"
        disabled={!activeSessionId}
        onClick={onDeleteSession}
        title="Elimina chat"
        aria-label="Elimina chat"
        style={iconButton(tc, !activeSessionId)}
      >
        🗑
      </button>
      <button
        type="button"
        disabled={!activeSessionId}
        onClick={onCompactSession}
        title={ctxPct != null ? `Compatta chat — context usato: ${ctxPct}%` : "Compatta chat"}
        aria-label={ctxPct != null ? `Compatta chat (context ${ctxPct}%)` : "Compatta chat"}
        style={{
          ...iconButton(tc, !activeSessionId),
          // Larghezza dinamica: il bottone si allarga per icona + badge percentuale
          // senza tagliare, anche a 4 cifre (es. 1952%).
          width: "auto",
          height: 30,
          minWidth: 30,
          maxWidth: "none",
          flex: "0 0 auto",
          display: "inline-flex",
          alignItems: "center",
          justifyContent: "center",
          gap: 4,
          paddingInline: ctxPct != null ? 10 : 0,
          whiteSpace: "nowrap",
          overflow: "hidden",
        }}
      >
        <span>⌁</span>
        {ctxPct != null && (
          <span style={{ fontSize: 10, fontWeight: 600, color: coloreCtx, lineHeight: 1 }}>{ctxPct}%</span>
        )}
      </button>
    </div>
  );
}
