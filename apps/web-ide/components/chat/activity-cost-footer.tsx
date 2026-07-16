"use client";

// Footer costo-per-provider del nastro attivita' (ADR 0037 sez. 2).
//
// Aggrega i token per (provider, model) dalle trace del run (punto unico
// aggregateTokensByProvider) e PREZZA col catalogo /api/models (usePricingCatalog
// di provider-badge.tsx). NIENTE prezzi hardcoded (regola G): se il catalogo non
// ha la entry, il costo di quel bucket e' 0 e restano solo i token. NON usa
// lib/model-catalog.ts (prezzi hardcoded, deprecato dall'ADR).
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

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Costo USD di un bucket dato il catalogo prezzi (0 se entry assente).
 *
 *  I nomi dei campi seguono il wire di `/api/models` (camelCase, punto unico
 *  `lib/api/models.ts`): sono obbligatori, quindi NIENTE `?? 0` a valle. Il
 *  fallback a zero e' ammesso in UN solo caso esplicito — il modello non e' nel
 *  catalog — e non puo' piu' mascherare un campo letto col nome sbagliato. */
function bucketCost(bucket: ProviderTokenBucket, catalog: ModelPricingEntry[]): number {
  const entry = catalog.find((e) => e.provider === bucket.provider && e.model === bucket.model);
  if (!entry) return 0;
  const inCost = entry.inputCostPerMillionTokens * (bucket.inputTokens / 1_000_000);
  const outCost = entry.outputCostPerMillionTokens * (bucket.outputTokens / 1_000_000);
  return inCost + outCost;
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
