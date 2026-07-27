"use client";

// Badge provider/modello colorato per le card AgentMetaStep.
//
// Funzione del badge:
//   1. Mostra il "provider/model" usato nel turno (visibilita' a colpo d'occhio
//      quando il routing fa cascade fallback tra modelli diversi).
//   2. Colore di background = mappa per PROVIDER (anthropic/openai/google/...).
//   3. Tonalita' opacita' = scala per COSTO del modello (input+output / 2M tok),
//      cosi' a parita' di provider un modello costoso e' visivamente piu'
//      "marcato" di uno economico.
//   4. Tooltip ricco: cost in/out USD, ctx window, capabilities.
//
// Pricing source: hook `useModelPricing` legge `/api/models` (gia' esistente,
// ritorna ai_price_catalog). Cache 5 min lato client per non chiamare l'endpoint
// ad ogni meta-step.

import { useEffect, useState } from "react";

// Normalizzazione provider, colori di brand, etichetta e conversione rgba vivono
// nel punto unico provider-icon-logic.ts (regola L). Qui li importiamo per l'uso
// interno e li ri-esportiamo per i call site storici che li prendono da
// "./provider-badge".
import {
  providerBaseColor,
  providerLabel,
  rgba,
} from "./provider-icon-logic";
import { listModelCatalog, type ModelCatalogEntry } from "../../lib/api/models";

export { providerBaseColor, providerLabel, rgba };

/** Il catalogo prezzi e' quello di `/api/models`: un solo wire, un solo tipo
 *  (regola L). Alias verso il punto unico `lib/api/models.ts` per i call site
 *  storici che importano `ModelPricingEntry` da qui. */
export type ModelPricingEntry = ModelCatalogEntry;

// ── Cache pricing client-side (TTL 5 min) ──────────────────────────────────
let _pricingCache: { loadedAt: number; entries: ModelCatalogEntry[] } | null =
  null;
const PRICING_TTL_MS = 5 * 60 * 1000;

async function fetchPricing(): Promise<ModelCatalogEntry[]> {
  const now = Date.now();
  if (_pricingCache && now - _pricingCache.loadedAt < PRICING_TTL_MS) {
    return _pricingCache.entries;
  }
  try {
    // listModelCatalog = punto unico di fetch (passa da fetchJson/_shared.ts).
    const { models } = await listModelCatalog();
    const entries = models ?? [];
    _pricingCache = { loadedAt: now, entries };
    return entries;
  } catch {
    return _pricingCache?.entries ?? [];
  }
}

/** Hook che restituisce l'INTERO catalogo pricing (cache 5 min condivisa con
 *  useModelPricing). Punto unico per prezzare piu' modelli in un colpo senza
 *  chiamare useModelPricing in un loop dinamico (violerebbe le regole hooks).
 *  Usato dal footer costo-per-provider del nastro attivita' (ADR 0037). */
export function usePricingCatalog(): ModelPricingEntry[] {
  const [entries, setEntries] = useState<ModelPricingEntry[]>(
    _pricingCache?.entries ?? [],
  );
  useEffect(() => {
    let cancelled = false;
    void fetchPricing().then((e) => {
      if (!cancelled) setEntries(e);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  return entries;
}

/** Hook che restituisce la entry del catalogo per il (provider, model) dato. */
export function useModelPricing(
  provider: string | null | undefined,
  model: string | null | undefined,
): ModelPricingEntry | null {
  const [entry, setEntry] = useState<ModelPricingEntry | null>(null);
  useEffect(() => {
    if (!provider || !model) {
      setEntry(null);
      return;
    }
    let cancelled = false;
    void fetchPricing().then((entries) => {
      if (cancelled) return;
      const found = entries.find(
        (e) => e.provider === provider && e.model === model,
      );
      setEntry(found ?? null);
    });
    return () => {
      cancelled = true;
    };
  }, [provider, model]);
  return entry;
}

/**
 * Calcola l'intensita' (alpha 0.25 → 0.95) del fondo in base al costo del
 * modello. Scala logaritmica: modelli economici (≤$1/M) sono molto trasparenti,
 * modelli costosi (≥$50/M) molto opachi. Esportata come punto unico (regola L):
 * ProviderIcon la usa come dimensione ORTOGONALE alla tinta per-modello (il
 * costo modula solo l'opacita', la tinta distingue il modello).
 */
export function alphaFromCost(entry: ModelPricingEntry | null): number {
  if (!entry) return 0.55;
  const inCost = entry.inputCostPerMillionTokens;
  const outCost = entry.outputCostPerMillionTokens;
  // Output cost pesa di piu' (3x) — riflette il costo dominante nelle agent runs.
  const weighted = inCost + 3 * outCost;
  if (weighted <= 0) return 0.30; // gratis / locale
  // Scala log: $0.5 -> 0.30, $5 -> 0.55, $50 -> 0.80, $500 -> 0.95
  const log = Math.log10(Math.max(weighted, 0.1));
  const norm = Math.min(1, Math.max(0, (log + 0.3) / 3.0));
  return 0.30 + 0.65 * norm;
}

/**
 * Badge inline: "anthropic/claude-sonnet-4-6" con sfondo colorato per
 * provider + tonalita' per costo. Tooltip ricco al hover.
 */
export function ProviderBadge({
  provider,
  model,
}: {
  provider: string | null | undefined;
  model: string | null | undefined;
}) {
  const entry = useModelPricing(provider, model);

  if (!provider && !model) return null;
  const base = providerBaseColor(provider);
  const label = providerLabel(provider);
  const alpha = alphaFromCost(entry);
  const bg = rgba(base, alpha);
  // Bordi piu' opachi per leggibilita'.
  const border = rgba(base, Math.min(0.95, alpha + 0.2));

  const tooltipParts: string[] = [];
  if (entry?.displayName) tooltipParts.push(entry.displayName);
  if (entry) {
    tooltipParts.push(`In: $${entry.inputCostPerMillionTokens.toFixed(2)}/M tok`);
    tooltipParts.push(`Out: $${entry.outputCostPerMillionTokens.toFixed(2)}/M tok`);
  }
  if (entry?.contextWindow) {
    tooltipParts.push(`ctx ${entry.contextWindow.toLocaleString()}`);
  }
  if (entry?.performanceTier) {
    tooltipParts.push(`tier: ${entry.performanceTier}`);
  }
  const tooltip = tooltipParts.length
    ? tooltipParts.join(" · ")
    : `${label} / ${model ?? "?"}`;

  return (
    <span
      title={tooltip}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 4,
        padding: "1px 7px",
        borderRadius: 10,
        background: bg,
        border: `1px solid ${border}`,
        fontSize: 10.5,
        fontFamily: 'var(--font-mono)',
        color: alpha > 0.6 ? "#fff" : "#1a1a1a",
        whiteSpace: "nowrap",
        maxWidth: 280,
        overflow: "hidden",
        textOverflow: "ellipsis",
      }}
    >
      <span style={{ fontWeight: 700, opacity: 0.9 }}>{label}</span>
      <span style={{ opacity: 0.55 }}>/</span>
      <span>{model ?? "?"}</span>
    </span>
  );
}
