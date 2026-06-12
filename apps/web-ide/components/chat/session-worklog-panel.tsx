"use client";

import { useCallback, useState } from "react";

import { getSessionWorklog } from "../../lib/api/chat";
import { useThemeColors } from "../../lib/theme";

/** Pannello collassabile "Cosa e' stato fatto in questa sessione": mostra il
    digest provider-neutro della storia di lavoro (mig 0411) — file toccati,
    comandi con esito, errori, tentativi falliti, decisioni. Carica on-demand al
    primo apri (niente costo se l'utente non lo apre). Lo stesso testo che guida
    l'LLM, reso visibile all'utente per chiudere il "non si capisce cosa e' stato
    fatto". */
export function SessionWorklogPanel({ sessionId }: { sessionId: string | null }) {
  const tc = useThemeColors();
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [block, setBlock] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const toggle = useCallback(async () => {
    if (!sessionId) return;
    if (open) {
      setOpen(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const r = await getSessionWorklog(sessionId);
      setBlock(r.renderedBlock ?? "");
      setOpen(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore nel caricamento");
    } finally {
      setLoading(false);
    }
  }, [sessionId, open]);

  // Ricarica al riapri se gia' aperto in passato (lo stato evolve durante la sessione).
  const reload = useCallback(async () => {
    if (!sessionId) return;
    setLoading(true);
    setError(null);
    try {
      const r = await getSessionWorklog(sessionId);
      setBlock(r.renderedBlock ?? "");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore nel caricamento");
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  if (!sessionId) return null;

  return (
    <div style={{ marginBottom: 8 }}>
      <button
        onClick={() => void toggle()}
        disabled={loading}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 6,
          padding: "4px 10px",
          borderRadius: 6,
          border: `1px solid ${tc.border}`,
          background: tc.bgInput,
          color: tc.textSecondary,
          fontSize: 12,
          cursor: loading ? "default" : "pointer",
          fontFamily: "inherit",
        }}
      >
        <span>{open ? "▾" : "▸"}</span>
        {loading ? "Caricamento..." : "Cosa e' stato fatto in questa sessione"}
      </button>

      {error ? (
        <div style={{ color: tc.error, fontSize: 11, marginTop: 4 }}>{error}</div>
      ) : null}

      {open ? (
        <div
          style={{
            marginTop: 6,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            background: tc.bgCard,
            padding: "10px 12px",
          }}
        >
          {block && block.trim() ? (
            <pre
              style={{
                margin: 0,
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
                fontSize: 12,
                lineHeight: 1.5,
                color: tc.text,
                fontFamily: "inherit",
              }}
            >
              {block}
            </pre>
          ) : (
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              Nessuna attivita' registrata in questa sessione.
            </div>
          )}
          <button
            onClick={() => void reload()}
            disabled={loading}
            style={{
              marginTop: 8,
              padding: "3px 8px",
              borderRadius: 5,
              border: `1px solid ${tc.border}`,
              background: tc.bgInput,
              color: tc.textMuted,
              fontSize: 11,
              cursor: "pointer",
              fontFamily: "inherit",
            }}
          >
            Aggiorna
          </button>
        </div>
      ) : null}
    </div>
  );
}
