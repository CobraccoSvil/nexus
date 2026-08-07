"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import dynamic from "next/dynamic";
import type { UserProfile } from "../../lib/api-client";
import type { Theme } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";
import { iconButton } from "../../lib/icon-button-style";
import { ModalPortal } from "../modal-portal";
import { useDismissOnOutside } from "../../hooks/use-dismiss-on-outside";

// Stesso dynamic import che usava ide-shell: importarlo statico lo tirerebbe nel
// bundle dell'header, che e' sempre montato. Qui il selettore si carica solo
// all'apertura del pannello.
const ProfileSelector = dynamic(() => import("./profile-selector.lazy"), {
  // Fuori da React: nessun traduttore in scope. Tre puntini invece di una
  // parola, cosi' non c'e' lingua da sbagliare.
  loading: () => <div style={{ fontSize: 12, opacity: 0.6 }}>…</div>,
  ssr: false,
});

export interface ChatHeadSession {
  id: string;
  title: string;
}

export interface ChatHeadPopoverProps {
  tc: Theme;
  profiles: UserProfile[];
  selectedProfileId: string;
  onSelectProfile: (id: string) => void;
  onCreateProfile: () => void;
  sessions: ChatHeadSession[];
  activeSessionId: string | null;
  onSelectSession: (id: string) => void;
  onNewSession: () => void;
  onRenameSession: () => void;
  onDeleteSession: () => void;
  onCompactSession: () => void;
  /** Riempimento della context window dell'ultimo turno, 0-100. `null` se ignoto. */
  ctxPct: number | null;
}

const LARGHEZZA_PANNELLO = 300;
const MARGINE_BORDO = 8;

/**
 * Testata della chat raccolta in un pannello a comparsa.
 *
 * Perche' esiste: in riga, la testata teneva titolo + selettore profilo +
 * selettore sessione + quattro pulsanti. Su una colonna stretta non ci stavano e
 * il gruppo delle sessioni — l'unico elemento cedevole — finiva a larghezza ZERO:
 * le chat diventavano irraggiungibili senza allargare il pannello (misurato: 148px
 * richiesti, 0 disponibili a colonna 288px). Nell'header resta solo questo
 * trigger, che tronca con ellissi e non puo' sfondare a nessuna larghezza.
 *
 * Il pannello e' reso via ModalPortal e posizionato in coordinate viewport:
 * l'antenato della colonna AI ha `overflow: hidden` e lo taglierebbe.
 */
export function ChatHeadPopover({
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
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [coord, setCoord] = useState<{ top: number; left: number } | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const pannelloRef = useRef<HTMLDivElement>(null);

  const chiudi = useCallback(() => setOpen(false), []);
  useDismissOnOutside(open, [triggerRef, pannelloRef], chiudi);

  // Ancoraggio al trigger in coordinate viewport, rientrando nei bordi.
  const calcolaPosizione = useCallback(() => {
    const t = triggerRef.current;
    if (!t) return;
    const r = t.getBoundingClientRect();
    const massimoLeft = window.innerWidth - LARGHEZZA_PANNELLO - MARGINE_BORDO;
    setCoord({
      top: Math.round(r.bottom + 4),
      left: Math.round(Math.max(MARGINE_BORDO, Math.min(r.left, massimoLeft))),
    });
  }, []);

  useEffect(() => {
    if (!open) return;
    calcolaPosizione();
    // In coordinate viewport il pannello non segue il trigger da solo: se la
    // finestra cambia o qualcosa scorre, l'ancoraggio va rifatto.
    const aggiorna = () => calcolaPosizione();
    window.addEventListener("resize", aggiorna);
    window.addEventListener("scroll", aggiorna, true);
    return () => {
      window.removeEventListener("resize", aggiorna);
      window.removeEventListener("scroll", aggiorna, true);
    };
  }, [open, calcolaPosizione]);

  const sessioneAttiva = sessions.find((s) => s.id === activeSessionId) ?? null;
  const etichetta = sessioneAttiva?.title || (sessions.length === 0 ? "Nessuna chat" : "Chat");
  const coloreCtx =
    ctxPct == null ? tc.textMuted : ctxPct >= 90 ? tc.error : ctxPct >= 70 ? tc.warning : tc.textMuted;

  const vociMenu: { testo: string; icona: string; azione: () => void; attivo: boolean }[] = [
    { testo: "Nuova chat", icona: "＋", azione: onNewSession, attivo: true },
    { testo: "Rinomina chat", icona: "✎", azione: onRenameSession, attivo: !!activeSessionId },
    { testo: "Compatta chat", icona: "⌁", azione: onCompactSession, attivo: !!activeSessionId },
    { testo: "Elimina chat", icona: "🗑", azione: onDeleteSession, attivo: !!activeSessionId },
  ];

  const titoloSezione: React.CSSProperties = {
    fontSize: 10,
    fontWeight: 700,
    letterSpacing: 0.4,
    textTransform: "uppercase",
    color: tc.textMuted,
    marginBottom: 6,
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        title={t("chat.testataChatProfiloSessioni")}
        aria-haspopup="dialog"
        aria-expanded={open}
        style={{
          ...iconButton(tc, false, open),
          // Il trigger e' l'unico elemento della testata rimasto in riga: deve
          // poter cedere fino a troncare, mai spingere l'header oltre la colonna.
          width: "auto",
          minWidth: 0,
          maxWidth: "100%",
          flexShrink: 1,
          gap: 6,
          paddingInline: 8,
          fontSize: 12,
          overflow: "hidden",
        }}
      >
        <span
          style={{
            minWidth: 0,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            fontWeight: 600,
          }}
        >
          {etichetta}
        </span>
        {ctxPct != null && (
          <span style={{ fontSize: 10, fontWeight: 600, color: coloreCtx, lineHeight: 1, flexShrink: 0 }}>
            {ctxPct}%
          </span>
        )}
        <span style={{ flexShrink: 0, opacity: 0.7 }}>{open ? "▴" : "▾"}</span>
      </button>

      {open && coord && (
        <ModalPortal>
          <div
            ref={pannelloRef}
            role="dialog"
            aria-label={t("chat.testataChat")}
            style={{
              position: "fixed",
              top: coord.top,
              left: coord.left,
              width: LARGHEZZA_PANNELLO,
              // Senza questo, `width` non comprende padding e bordo: il pannello
              // misurava 326px contro i 300 su cui calcolo `left`, e sfondava il
              // bordo destro di 18px (misurato).
              boxSizing: "border-box",
              maxWidth: `calc(100vw - ${MARGINE_BORDO * 2}px)`,
              maxHeight: "70vh",
              overflowY: "auto",
              background: tc.bgCard,
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
              boxShadow: "0 12px 32px rgba(0,0,0,0.18)",
              padding: 12,
              zIndex: 9000,
              display: "flex",
              flexDirection: "column",
              gap: 14,
            }}
          >
            <div>
              <div style={titoloSezione}>{t("chat.head.profile")}</div>
              <ProfileSelector
                profiles={profiles}
                selectedProfileId={selectedProfileId}
                onSelect={onSelectProfile}
                onCreateNew={onCreateProfile}
                style={{ width: "100%", minWidth: 0 }}
              />
            </div>

            <div>
              <div style={titoloSezione}>{t("chat.head.session")}</div>
              {/* Tendina come quella del profilo: l'elenco a pulsanti cresceva
                  con le sessioni e sbilanciava il pannello. `boxSizing` non e'
                  decorativo qui: con width 100% il padding si sommerebbe e il
                  select sfonderebbe il pannello. */}
              <select
                value={activeSessionId ?? ""}
                onChange={(e) => {
                  const id = e.target.value;
                  if (id) onSelectSession(id);
                }}
                disabled={sessions.length === 0}
                title={t("chat.head.selectSession")}
                aria-label={t("chat.head.selectSession")}
                style={{
                  width: "100%",
                  minWidth: 0,
                  boxSizing: "border-box",
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 6,
                  padding: "4px 6px",
                  fontSize: 12,
                  fontFamily: "inherit",
                  color: sessions.length === 0 ? tc.textMuted : tc.text,
                  cursor: sessions.length === 0 ? "not-allowed" : "pointer",
                }}
              >
                {sessions.length === 0 ? (
                  <option value="">{t("chat.head.noChat")}</option>
                ) : (
                  sessions.map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.title}
                    </option>
                  ))
                )}
              </select>
            </div>

            <div>
              <div style={titoloSezione}>{t("chat.head.actions")}</div>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                {vociMenu.map((v) => (
                  <button
                    key={v.testo}
                    type="button"
                    disabled={!v.attivo}
                    onClick={() => {
                      v.azione();
                      setOpen(false);
                    }}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      textAlign: "left",
                      padding: "6px 8px",
                      borderRadius: 6,
                      border: "1px solid transparent",
                      background: "transparent",
                      color: v.attivo ? tc.text : tc.textMuted,
                      cursor: v.attivo ? "pointer" : "not-allowed",
                      fontSize: 12,
                      fontFamily: "inherit",
                    }}
                  >
                    <span style={{ width: 16, textAlign: "center", flexShrink: 0 }}>{v.icona}</span>
                    <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {v.testo}
                    </span>
                    {v.testo === "Compatta chat" && ctxPct != null && (
                      <span style={{ marginLeft: "auto", fontSize: 10, fontWeight: 600, color: coloreCtx }}>
                        {ctxPct}%
                      </span>
                    )}
                  </button>
                ))}
              </div>
            </div>
          </div>
        </ModalPortal>
      )}
    </>
  );
}
