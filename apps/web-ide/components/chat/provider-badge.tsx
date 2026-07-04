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

// ── Brand colors per provider ──────────────────────────────────────────────
// Coerenti con la barra status providers in alto a destra dell'IDE (OpenAI,
// Anthropic, Google, DeepSeek, Mistral). Hex base + scala per tonalita'.
const PROVIDER_COLORS: Record<string, { base: string; label: string }> = {
  anthropic: { base: "#cc785c", label: "Anthropic" },
  openai: { base: "#10a37f", label: "OpenAI" },
  google: { base: "#4285f4", label: "Google" },
  deepseek: { base: "#7c3aed", label: "DeepSeek" },
  mistral: { base: "#ff7000", label: "Mistral" },
  vllm: { base: "#737373", label: "vLLM" },
  ollama: { base: "#737373", label: "Ollama" },
  local: { base: "#737373", label: "Local" },
  unknown: { base: "#94a3b8", label: "?" },
};

/** Colore brand base (#RRGGBB) per provider. Riusato dalle card meta-step per
 *  l'accento della riga (colore differente in base al provider). */
export function providerBaseColor(provider: string | null | undefined): string {
  const key = (provider ?? "unknown").toLowerCase();
  return (PROVIDER_COLORS[key] ?? PROVIDER_COLORS.unknown).base;
}

/** Etichetta leggibile del provider (brand). Punto unico riusato da ProviderIcon
 *  per il tooltip. Per provider ignoti ritorna la sigla iniziale maiuscola. */
export function providerLabel(provider: string | null | undefined): string {
  const key = (provider ?? "unknown").toLowerCase();
  const entry = PROVIDER_COLORS[key];
  if (entry) return entry.label;
  return provider ? provider.charAt(0).toUpperCase() + provider.slice(1) : "?";
}

export interface ModelPricingEntry {
  provider: string;
  model: string;
  display_name?: string;
  input_cost_per_million_tokens?: number;
  output_cost_per_million_tokens?: number;
  context_window?: number;
  performance_tier?: string;
}

// ── Cache pricing client-side (TTL 5 min) ──────────────────────────────────
let _pricingCache: { loadedAt: number; entries: ModelPricingEntry[] } | null =
  null;
const PRICING_TTL_MS = 5 * 60 * 1000;

async function fetchPricing(): Promise<ModelPricingEntry[]> {
  const now = Date.now();
  if (_pricingCache && now - _pricingCache.loadedAt < PRICING_TTL_MS) {
    return _pricingCache.entries;
  }
  try {
    const res = await fetch("/api/models", { credentials: "include" });
    if (!res.ok) return _pricingCache?.entries ?? [];
    const data = (await res.json()) as { models?: ModelPricingEntry[] };
    const entries = data.models ?? [];
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
 * Calcola la tonalita' (alpha 0.25 → 0.95) di un colore in base al costo
 * del modello. Scala logaritmica: modelli economici (≤$1/M) sono molto
 * trasparenti, modelli costosi (≥$50/M) molto opachi. Esportata come punto
 * unico (regola L): riusata da ProviderIcon per distinguere modelli diversi
 * dello stesso provider con una tonalita' del brand.
 */
export function alphaFromCost(entry: ModelPricingEntry | null): number {
  if (!entry) return 0.55;
  const inCost = entry.input_cost_per_million_tokens ?? 0;
  const outCost = entry.output_cost_per_million_tokens ?? 0;
  // Output cost pesa di piu' (3x) — riflette il costo dominante nelle agent runs.
  const weighted = inCost + 3 * outCost;
  if (weighted <= 0) return 0.30; // gratis / locale
  // Scala log: $0.5 -> 0.30, $5 -> 0.55, $50 -> 0.80, $500 -> 0.95
  const log = Math.log10(Math.max(weighted, 0.1));
  const norm = Math.min(1, Math.max(0, (log + 0.3) / 3.0));
  return 0.30 + 0.65 * norm;
}

/** Converte hex (#RRGGBB) + alpha in `rgba(r,g,b,a)`. Esportata (regola L):
 *  riusata da ProviderIcon per tingere il mark col brand + tonalita' costo. */
export function rgba(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return hex;
  const v = m[1];
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha.toFixed(2)})`;
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
  const providerKey = (provider ?? "unknown").toLowerCase();
  const palette = PROVIDER_COLORS[providerKey] ?? PROVIDER_COLORS.unknown;
  const alpha = alphaFromCost(entry);
  const bg = rgba(palette.base, alpha);
  // Bordi piu' opachi per leggibilita'.
  const border = rgba(palette.base, Math.min(0.95, alpha + 0.2));

  const tooltipParts: string[] = [];
  if (entry?.display_name) tooltipParts.push(entry.display_name);
  if (entry?.input_cost_per_million_tokens != null) {
    tooltipParts.push(
      `In: $${entry.input_cost_per_million_tokens.toFixed(2)}/M tok`,
    );
  }
  if (entry?.output_cost_per_million_tokens != null) {
    tooltipParts.push(
      `Out: $${entry.output_cost_per_million_tokens.toFixed(2)}/M tok`,
    );
  }
  if (entry?.context_window) {
    tooltipParts.push(`ctx ${entry.context_window.toLocaleString()}`);
  }
  if (entry?.performance_tier) {
    tooltipParts.push(`tier: ${entry.performance_tier}`);
  }
  const tooltip = tooltipParts.length
    ? tooltipParts.join(" · ")
    : `${palette.label} / ${model ?? "?"}`;

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
      <span style={{ fontWeight: 700, opacity: 0.9 }}>{palette.label}</span>
      <span style={{ opacity: 0.55 }}>/</span>
      <span>{model ?? "?"}</span>
    </span>
  );
}
