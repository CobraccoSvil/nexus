import { API_BASE, fetchJson } from "./_shared";

// ── Model Catalog ──────────────────────────────────────────────────────────

export interface ModelCatalogEntry {
  provider: string;
  model: string;
  displayName: string;
  inputCostPerMillion: number;
  outputCostPerMillion: number;
  currency: string;
  performanceTier: "light" | "medium" | "heavy";
  speedTier: "fast" | "medium" | "slow";
  capabilities: string[];
  contextWindow: number;
  supportsToolUse: boolean;
  batchDiscountPct: number;
  isFeatured: boolean;
  isEnabled: boolean;
}

export async function getModels(provider?: string): Promise<{ models: ModelCatalogEntry[] }> {
  const url = provider
    ? `${API_BASE}/api/models?provider=${encodeURIComponent(provider)}`
    : `${API_BASE}/api/models`;
  return fetchJson(url);
}

// ── Model Catalog (per dropdown billing) ───────────────────────────────────

export interface ModelCatalogItem {
  provider: string;
  model: string;
  displayName: string;
  inputCostPerMillionTokens: number;
  outputCostPerMillionTokens: number;
  currency: string;
  performanceTier: string;
  speedTier: string;
  contextWindow: number;
  isFeatured: boolean;
  isEnabled: boolean;
}

export async function listModelCatalog(): Promise<{ models: ModelCatalogItem[] }> {
  return fetchJson(`${API_BASE}/api/models`);
}
