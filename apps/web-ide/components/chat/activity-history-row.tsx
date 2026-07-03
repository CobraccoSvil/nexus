"use client";

// Riga storica compatta di un turno passato (ADR 0037): badge stato + trail
// provider colorato (un pallino per segmento, nell'ordine di esecuzione) +
// token/costo, espandibile per rivedere il nastro completo. Solo l'ultimo turno
// resta espanso; i precedenti sono collassati.
//
// La riga compone lo stream UNA volta dai dati grezzi del run (metaSteps + steps
// + traces) tramite il PUNTO UNICO composeActivityStream (regola L): lo stesso
// modello alimenta sia il trail/riepilogo (collassato) sia il nastro completo +
// footer costo (espanso). Nessuna derivazione provider/costo duplicata qui.
// Nessun fetch: i dati arrivano gia' dalle props (memoria/DB via useChat).
//
// Densita': a larghezze strette cede il TESTO del turno (nx-as-hist-query) e i
// token nel costo (nx-as-hist-tokens); restano stato, trail e costo. Il trail
// provider e' colorato anche da collassato (invariante multi-provider).
//
// Stile: inline + useThemeColors, niente Tailwind, niente emoji nei sorgenti.

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { providerBaseColor } from "./provider-badge";
import { ActivityStreamView } from "./activity-stream";
import { ActivityCostFooter } from "./activity-cost-footer";
import {
  composeActivityStream,
  type ActivityStream,
  type FoldThreshold,
} from "../../lib/use-chat/activity-stream";
import { useResolvedRunSteps } from "../../lib/use-chat/use-run-steps";
import type { MetaStepEntry } from "../../lib/use-chat/types";
import type { AgentStep, AITraceEvent } from "../../lib/api/agent";

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Esito sintetico del turno storico (ok/errore) dallo stato del run. */
function statusTone(runStatus: string | undefined): "ok" | "err" | "neutral" {
  if (!runStatus) return "neutral";
  if (runStatus === "completed" || runStatus === "completed_verified") {
    return "ok";
  }
  if (
    runStatus === "failed" ||
    runStatus === "failed_diagnosed" ||
    runStatus === "timed_out" ||
    runStatus === "loop_aborted"
  ) {
    return "err";
  }
  return "neutral";
}

/** Trail dei provider toccati nel turno (uno per segmento, in ordine, senza
 *  doppioni consecutivi). */
function providerTrail(stream: ActivityStream): string[] {
  const trail: string[] = [];
  for (const seg of stream.segments) {
    if (trail[trail.length - 1] !== seg.provider) trail.push(seg.provider);
  }
  return trail;
}

export function ActivityHistoryRow({
  runId,
  metaSteps,
  steps,
  traces,
  foldThreshold,
  runStatus,
  query,
  totalTokens,
  totalCostUsd,
  defaultExpanded = false,
  tc,
}: {
  /** id del run: usato per il lazy-fetch degli step storici mancanti. */
  runId: string;
  metaSteps: MetaStepEntry[];
  steps: AgentStep[];
  /** Trace del SOLO run del turno (gia' filtrate dal parent). */
  traces: AITraceEvent[];
  foldThreshold: FoldThreshold;
  runStatus?: string;
  /** Testo utente del turno (per il riepilogo compatto). */
  query?: string;
  /** Token/costo dal messaggio persistito (nessun fetch, nessun ricalcolo). */
  totalTokens?: number;
  totalCostUsd?: number;
  defaultExpanded?: boolean;
  tc: ThemeColors;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  // Lazy-fetch degli step storici SOLO quando la riga e' espansa (evita N fetch
  // al bootstrap per ogni turno passato collassato). Se gli step sono gia'
  // presenti, niente fetch. Il trail (dai meta_step) resta corretto anche
  // prima del fetch.
  const resolvedSteps = useResolvedRunSteps(runId, steps, expanded);
  // Punto unico di composizione (regola L): un solo modello per trail + nastro.
  const stream = composeActivityStream(metaSteps, resolvedSteps, traces, foldThreshold);
  const tone = statusTone(runStatus);
  const trail = providerTrail(stream);

  const toneColor = tone === "ok" ? "#22c55e" : tone === "err" ? tc.error : tc.textMuted;
  const toneGlyph = tone === "ok" ? "✓" : tone === "err" ? "✗" : "•";

  return (
    <div
      style={{
        border: `1px solid ${tc.border}`,
        borderRadius: 10,
        background: tc.bgCard,
        overflow: "hidden",
        minWidth: 0,
      }}
      data-testid="activity-history-row"
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        style={{
          width: "100%",
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "8px 10px",
          background: "none",
          border: "none",
          cursor: "pointer",
          textAlign: "left",
          minWidth: 0,
        }}
      >
        <span aria-hidden style={{ color: tc.textMuted, fontFamily: "var(--font-mono)", fontSize: 11, flexShrink: 0 }}>
          {expanded ? "▾" : "▸"}
        </span>
        {/* Badge stato (invariante) */}
        <span
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 4,
            fontSize: 11,
            fontWeight: 700,
            color: toneColor,
            background: `${toneColor}1f`,
            border: `1px solid ${toneColor}66`,
            borderRadius: 6,
            padding: "1px 7px",
            flexShrink: 0,
          }}
        >
          {toneGlyph}
        </span>
        {/* Testo del turno (cede per primo) */}
        {query && (
          <span
            className="nx-as-hist-query"
            style={{
              flex: 1,
              minWidth: 0,
              color: tc.textMuted,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {query}
          </span>
        )}
        {/* Trail provider colorato (invariante: pallini colorati anche da
            collassato -> multi-provider percepibile senza espandere) */}
        <span style={{ display: "inline-flex", alignItems: "center", gap: 3, flexShrink: 0 }}>
          {trail.map((p, i) => (
            <span key={`trail-${i}`} style={{ display: "inline-flex", alignItems: "center", gap: 3 }}>
              {i > 0 && <span style={{ opacity: 0.5, color: tc.textMuted }}>{"→"}</span>}
              <span
                title={p}
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: providerBaseColor(p),
                  display: "inline-block",
                }}
              />
            </span>
          ))}
        </span>
        {/* Costo/token dal messaggio persistito */}
        {(totalTokens != null || totalCostUsd != null) && (
          <span
            style={{
              marginLeft: query ? 0 : "auto",
              fontFamily: "var(--font-mono)",
              fontSize: 10.5,
              color: tc.textMuted,
              flexShrink: 0,
            }}
          >
            {totalTokens != null && (
              <span className="nx-as-hist-tokens">
                {totalTokens.toLocaleString("it-IT")} tok{" · "}
              </span>
            )}
            {totalCostUsd != null && <b style={{ color: tc.text }}>${totalCostUsd.toFixed(3)}</b>}
          </span>
        )}
      </button>

      {expanded && !stream.empty && (
        <div style={{ borderTop: `1px solid ${tc.border}`, padding: "0 2px 4px", minWidth: 0 }}>
          <ActivityStreamView stream={stream} tc={tc} />
          {traces.length > 0 && <ActivityCostFooter traces={traces} tc={tc} />}
        </div>
      )}
    </div>
  );
}
