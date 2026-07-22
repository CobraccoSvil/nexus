"use client";

// Footer costo-per-provider del nastro attivita' (ADR 0037 sez. 2).
//
// Aggrega i token per (provider, model) dalle trace del run (punto unico
// aggregateTokensByProvider) e PREZZA col catalogo /api/models (usePricingCatalog
// di provider-badge.tsx). NIENTE prezzi hardcoded (regola G): se il catalogo non
// ha la entry, il costo di quel bucket e' 0 e restano solo i token.
//
// La formula del costo vive in lib/model-catalog.ts (costFromCatalog): quel file
// non contiene piu' il listino scritto a mano da cui l'ADR prendeva le distanze
// — ora e' solo il calcolo, alimentato dal catalogo del DB.
//
// Densita': a larghezze strette il NOME provider nei costi cede (classe
// nx-as-cost-provider-name); restano barra + numeri.

import { useThemeColors } from "../../lib/theme";
import {
  providerBaseColor,
  usePricingCatalog,
  type ModelPricingEntry,
} from "./provider-badge";
import {
  aggregateTokensByProvider,
  type ProviderTokenBucket,
} from "../../lib/use-chat/activity-stream";
import type { AITraceEvent } from "../../lib/api/agent";
import { costFromCatalog, findCatalogEntry } from "../../lib/model-catalog";

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Costo USD di un bucket dato il catalogo prezzi (0 se entry assente).
 *
 *  Il calcolo e' quello del punto unico `costFromCatalog` (regola L): questa
 *  funzione era una seconda implementazione della stessa formula, e sarebbe
 *  divergente dall'altra alla prima modifica (i token di cache, per esempio,
 *  qui non erano tariffati affatto). Qui resta solo l'adattamento del bucket
 *  alla firma comune e la scelta — voluta, locale a questo footer — di trattare
 *  un modello non a catalogo come contributo zero anziche' nasconderlo. */
function bucketCost(bucket: ProviderTokenBucket, catalog: ModelPricingEntry[]): number {
  const entry = findCatalogEntry(catalog, bucket.provider, bucket.model);
  return costFromCatalog(entry, bucket.inputTokens, bucket.outputTokens) ?? 0;
}

export function ActivityCostFooter({
  traces,
  tc,
}: {
  traces: AITraceEvent[];
  tc: ThemeColors;
}) {
  const catalog = usePricingCatalog();
  const buckets = aggregateTokensByProvider(traces);
  if (buckets.length === 0) return null;

  const totalTokens = buckets.reduce((s, b) => s + b.inputTokens + b.outputTokens, 0);
  const costs = buckets.map((b) => bucketCost(b, catalog));
  const totalCost = costs.reduce((s, c) => s + c, 0);
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
        {buckets.map((b, i) => {
          const frac = (b.inputTokens + b.outputTokens) / totalForBar;
          return (
            <span
              key={`bar-${i}`}
              style={{ width: `${(frac * 100).toFixed(1)}%`, background: providerBaseColor(b.provider) }}
            />
          );
        })}
      </span>
      {buckets.map((b, i) => {
        const color = providerBaseColor(b.provider);
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
              {b.provider}
            </span>
            <b style={{ color }}>${costs[i].toFixed(4)}</b>
          </span>
        );
      })}
      <span style={{ marginLeft: "auto" }}>
        <b style={{ color: tc.text }}>${totalCost.toFixed(4)}</b>
      </span>
    </div>
  );
}
