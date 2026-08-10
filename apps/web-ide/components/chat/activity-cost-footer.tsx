"use client";

// Footer costo-per-provider del nastro attivita' (ADR 0037 sez. 2).
//
// Le voci vengono dal LEDGER, dal perimetro del RUN (se stesso + i sub-run che
// ha dispatchato) e dalla stessa lettura che porta il totale mostrato accanto.
// Prima si componevano dalle TRACCE e si riprezzavano col catalogo /api/models,
// mentre il totale veniva gia' dal ledger: due fonti per due numeri che l'utente
// legge insieme, e infatti non tornavano — MISURATO il 10/08/2026, l'elenco
// dichiarava `openai $0.0000` (zero righe nel ledger nelle stesse 12 ore) e
// ometteva kimi e groq, che ne avevano 25.
//
// Il quando si legge sta nell'hook (`useRipartizioneRun`), il che si puo'
// dichiarare nel modulo puro (`use-run-cost-logic.ts`): qui resta solo la resa.
// NIENTE prezzi qui e niente ripiego sulle tracce: se il ledger non ha dato la
// ripartizione, il footer lo dice (regola Q) invece di ricomporne una da
// un'altra fonte, che e' esattamente il difetto appena chiuso.
//
// Densita': a larghezze strette il NOME provider nei costi cede (classe
// nx-as-cost-provider-name); restano barra + numeri.

import { useThemeColors } from "../../lib/theme";
import { providerBaseColor } from "./provider-badge";
import { etichetteVociCosto } from "../../lib/use-chat/activity-stream";
import { useRipartizioneRun } from "../../lib/use-chat/use-run-cost";
import { vistaCostoRun, type VoceCostoLedger } from "../../lib/use-chat/use-run-cost-logic";
import type { CurrentRunUsage } from "./token-usage-bar-logic";

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Il perimetro contabile del turno, dichiarato nel tooltip: senza, i numeri
 *  del footer e quelli del contatore sotto la chat si leggono come due misure
 *  della stessa cosa che non tornano (sullo stesso istante misurato l'08/08:
 *  $0,1272 il run, $2,6024 la conversazione). */
function perimetroLeggibile(runCount: number): string {
  const delegati = runCount - 1;
  if (delegati <= 0) return "questo turno, dal ledger";
  return delegati === 1
    ? "questo turno e il lavoro che ha delegato (1 sub-run), dal ledger"
    : `questo turno e il lavoro che ha delegato (${delegati} sub-run), dal ledger`;
}

export function ActivityCostFooter({
  runId,
  sessionId,
  ripartizioneNota,
  tc,
}: {
  runId: string;
  /** Serve al backend per autorizzare e per risolvere il pool del progetto:
   *  senza, il perimetro del run non e' chiedibile. */
  sessionId?: string;
  /** Il perimetro gia' letto dal contatore sotto la chat. Se e' di QUESTO run
   *  si usa quello — stessa fonte, e riletto al ritmo del run invece che fermo
   *  all'istante in cui il footer e' comparso. */
  ripartizioneNota?: CurrentRunUsage | null;
  tc: ThemeColors;
}) {
  const vista = vistaCostoRun(useRipartizioneRun(sessionId, runId, ripartizioneNota));

  if (vista.modo === "in_lettura") return null;
  if (vista.modo === "nessun_consumo") {
    // Non e' un footer vuoto: e' il ledger che per questo perimetro non ha
    // ancora righe finalizzate (il caso normale di un turno appena partito).
    return (
      <GuscioFooter tc={tc} titolo="Nessuna riga di ledger finalizzata per questo turno.">
        <span>nessun consumo registrato</span>
      </GuscioFooter>
    );
  }
  if (vista.modo === "non_disponibile") {
    return (
      <GuscioFooter tc={tc} titolo={`Contabilita' del turno non leggibile: ${vista.motivo}.`}>
        <span>costo del turno non leggibile</span>
      </GuscioFooter>
    );
  }

  const totale = (
    <span style={{ marginLeft: "auto" }}>
      tot. <b style={{ color: tc.text }}>${vista.totalCostUsd.toFixed(4)}</b>
    </span>
  );

  if (vista.modo === "solo_totale") {
    // Il backend ha dato il totale e non la sua ripartizione: si mostra quel che
    // c'e' e si dichiara quel che manca, senza ricomporlo da un'altra fonte.
    return (
      <GuscioFooter tc={tc} titolo={`Token e costo di ${perimetroLeggibile(vista.runCount)}.`}>
        <TokenTotali n={vista.totalTokens} tc={tc} />
        <span>ripartizione per provider non dichiarata</span>
        {totale}
      </GuscioFooter>
    );
  }

  const etichette = etichetteVociCosto(vista.voci);
  return (
    <GuscioFooter tc={tc} titolo={`Token e costo di ${perimetroLeggibile(vista.runCount)}.`}>
      <TokenTotali n={vista.totalTokens} tc={tc} />
      <BarraToken voci={vista.voci} totale={vista.totalTokens} tc={tc} />
      {vista.voci.map((v, i) => (
        <VoceCosto key={`cost-${i}`} voce={v} etichetta={etichette[i] ?? v.provider} />
      ))}
      {totale}
    </GuscioFooter>
  );
}

/** La riga del footer: stessa cornice per tutti i modi, cosi' un turno senza
 *  ripartizione non si legge come un turno senza footer. */
function GuscioFooter({
  tc,
  titolo,
  children,
}: {
  tc: ThemeColors;
  titolo: string;
  children: React.ReactNode;
}) {
  return (
    <div
      title={titolo}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        flexWrap: "wrap",
        padding: "7px 12px",
        borderTop: `1px solid ${tc.border}`,
        background: tc.bgInput,
        fontFamily: "var(--font-mono)",
        fontSize: 10.5,
        color: tc.textMuted,
        minWidth: 0,
      }}
    >
      {children}
    </div>
  );
}

function TokenTotali({ n, tc }: { n: number; tc: ThemeColors }) {
  return (
    <span>
      <b style={{ color: tc.text }}>{n.toLocaleString("it-IT")}</b> tok
    </span>
  );
}

/** Barra token per voce (colore brand del provider, proporzionale). La misura e'
 *  la stessa del totale accanto: i token che il ledger ha contato per quella
 *  coppia provider/modello. */
function BarraToken({
  voci,
  totale,
  tc,
}: {
  voci: VoceCostoLedger[];
  totale: number;
  tc: ThemeColors;
}) {
  const base = Math.max(totale, 1);
  return (
    <span
      style={{
        display: "flex",
        height: 8,
        width: 120,
        borderRadius: 5,
        overflow: "hidden",
        border: `1px solid ${tc.border}`,
        flexShrink: 0,
      }}
    >
      {voci.map((v, i) => (
        <span
          key={`bar-${i}`}
          style={{
            width: `${((v.tokens / base) * 100).toFixed(1)}%`,
            background: providerBaseColor(v.provider),
          }}
        />
      ))}
    </span>
  );
}

function VoceCosto({ voce, etichetta }: { voce: VoceCostoLedger; etichetta: string }) {
  const color = providerBaseColor(voce.provider);
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 5, color, minWidth: 0 }}>
      <span
        className="nx-as-cost-provider-name"
        style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
      >
        {etichetta}
      </span>
      <b style={{ color }}>${voce.costUsd.toFixed(4)}</b>
    </span>
  );
}
