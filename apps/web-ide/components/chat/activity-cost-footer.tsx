"use client";

// Footer costo-per-provider del nastro attivita' (ADR 0037 sez. 2).
//
// Compone le voci per (provider, model) dal punto unico providerCostBreakdown e
// le PREZZA col catalogo /api/models (usePricingCatalog di provider-badge.tsx).
// NIENTE prezzi hardcoded (regola G): se il catalogo non ha la entry, il costo di
// quella voce e' 0 e restano solo i token.
//
// Le trace da passare sono quelle di `tracesForRun`, che include i sub-run: una
// barra composta sulle sole trace del run padre dichiara una ripartizione e ne
// omette i provider usati solo dai figli (difetto misurato il 26/07/2026 —
// vedi `crates/mcp-core/src/run_lineage.rs`).
//
// La formula del costo vive in lib/model-catalog.ts (costFromCatalog) e con essa
// il prezzatore del bucket (bucketCost): quel file non contiene piu' il listino
// scritto a mano da cui l'ADR prendeva le distanze — ora e' solo il calcolo,
// alimentato dal catalogo del DB. Qui NON si ricalcola nulla: un prezzatore
// scritto in questo file non sarebbe raggiungibile da alcun test (il modulo tira
// dentro React) e potrebbe divergere in silenzio da quello misurato.
//
// Densita': a larghezze strette il NOME provider nei costi cede (classe
// nx-as-cost-provider-name); restano barra + numeri.

import { useThemeColors } from "../../lib/theme";
import { providerBaseColor, usePricingCatalog } from "./provider-badge";
import { etichetteVociCosto, providerCostBreakdown } from "../../lib/use-chat/activity-stream";
import type { AITraceEvent } from "../../lib/api/agent";
import { bucketCost } from "../../lib/model-catalog";

type ThemeColors = ReturnType<typeof useThemeColors>;

export function ActivityCostFooter({
  traces,
  tc,
}: {
  traces: AITraceEvent[];
  tc: ThemeColors;
}) {
  const catalog = usePricingCatalog();
  // Composizione dal punto unico (regola L): quali voci esistono e come si
  // sommano vive in `providerCostBreakdown`, testato senza React; qui resta il
  // solo rendering e il listino, che il punto unico non conosce (regola G).
  const { voci, totalTokens, totalCostUsd } = providerCostBreakdown(traces, (b) =>
    bucketCost(b, catalog),
  );
  if (voci.length === 0) return null;

  // Il provider da solo non distingue due voci dello stesso provider su modelli
  // diversi: il criterio sta col punto unico, qui resta la resa.
  const etichette = etichetteVociCosto(voci);
  const totalForBar = Math.max(totalTokens, 1);

  return (
    <div
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
      <span>
        <b style={{ color: tc.text }}>{totalTokens.toLocaleString("it-IT")}</b> tok
      </span>
      {/* Barra token per provider (colore brand, proporzionale) */}
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
        {voci.map((b, i) => {
          // Stessa misura del totale (`providerCostBreakdown.totalTokens`):
          // prompt LORDO + output, coi token di cache gia' dentro il primo.
          const frac = (b.inputTokens + b.outputTokens) / totalForBar;
          return (
            <span
              key={`bar-${i}`}
              style={{ width: `${(frac * 100).toFixed(1)}%`, background: providerBaseColor(b.provider) }}
            />
          );
        })}
      </span>
      {voci.map((b, i) => {
        const color = providerBaseColor(b.provider);
        const etichetta = etichette[i] ?? b.provider;
        return (
          <span
            key={`cost-${i}`}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 5,
              color,
              minWidth: 0,
            }}
          >
            <span
              className="nx-as-cost-provider-name"
              style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
            >
              {etichetta}
            </span>
            <b style={{ color }}>${b.costUsd.toFixed(4)}</b>
          </span>
        );
      })}
      {/* Il totale portava il solo importo: accanto a tre cifre per fornitore
          nella stessa riga, l'ultima si leggeva come un quarto fornitore senza
          nome invece che come la loro somma. L'etichetta e' letterale come il
          "tok" qui sopra -- questo footer non passa da i18n. */}
      <span style={{ marginLeft: "auto" }}>
        tot. <b style={{ color: tc.text }}>${totalCostUsd.toFixed(4)}</b>
      </span>
    </div>
  );
}
