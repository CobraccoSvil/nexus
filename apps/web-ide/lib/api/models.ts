import { API_BASE, fetchJson } from "./_shared";

// ── Model Catalog ──────────────────────────────────────────────────────────

export interface ModelCatalogEntry {
  provider: string;
  model: string;
  displayName: string;
  inputCostPerMillion: number;
  outputCostPerMillion: number;
  currency: string;
  performanceTier: "light" | "medium" | "high" | "heavy" | "frontier";
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

// ── Provider registry (fonte unica data-driven per la dashboard admin) ───────

export interface ProviderRegistryEntry {
  name: string;
  apiFormat: string;
  /** Setting della API key (null per provider senza key, es. vllm). */
  keySetting: string | null;
  enabledSetting: string | null;
  baseUrlSetting: string | null;
  baseUrlDefault: string | null;
  /** api_key | base_url | api_key_or_vertex */
  activation: string;
  supportsTools: boolean;
  isActive: boolean;
  sortOrder: number;
  /** URL console billing/keys (null = self-host). */
  billingUrl: string | null;
}

/**
 * Elenco dei provider del registry (nexus_provider_registry). Fonte unica per
 * la dashboard admin: da qui la UI deriva quali provider hanno una API key,
 * il criterio di attivazione e il link billing, senza hardcode.
 */
export async function getProviderRegistry(): Promise<{ providers: ProviderRegistryEntry[] }> {
  return fetchJson(`${API_BASE}/api/admin/provider-registry`);
}

/**
 * Modelli del catalog di un provider INCLUSI i disabilitati (a differenza di
 * `/api/models`): la dashboard admin li mostra per poterli abilitare.
 */
export async function getProviderModelsAdmin(
  provider?: string,
): Promise<{ models: ModelCatalogEntry[] }> {
  const url = provider
    ? `${API_BASE}/api/admin/provider-models?provider=${encodeURIComponent(provider)}`
    : `${API_BASE}/api/admin/provider-models`;
  return fetchJson(url);
}

/** Abilita/disabilita un modello del catalog (ai_price_catalog.is_enabled). */
export async function setModelEnabled(
  provider: string,
  model: string,
  enabled: boolean,
): Promise<{ ok: boolean; provider: string; model: string; enabled: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/provider-models/enabled`, {
    method: "PUT",
    body: JSON.stringify({ provider, model, enabled }),
  });
}

// ── Preview routing REALE (dalla matrice DB corrente) ────────────────────────

export interface RoutingPreviewEntry {
  intent: string;
  provider: string;
  model: string;
  inputCost: number;
  speed: string;
}

export interface RoutingPreviewResponse {
  mode: string;
  estimatedAvgCostInputPerMillion: number;
  routing: RoutingPreviewEntry[];
  error?: string;
}

/**
 * Anteprima del routing per una modalita' (veloce|economica|bilanciata|approfondita):
 * legge la matrice REALE nexus_routing_matrix (cache 60s), non un mirror hardcoded.
 */
export async function getRoutingPreview(mode: string): Promise<RoutingPreviewResponse> {
  return fetchJson(`${API_BASE}/api/models/routing-preview?mode=${encodeURIComponent(mode)}`);
}

/**
 * Forza un tick dell'auto-promoter: ricalcola le celle best-fit della matrice
 * statica dal catalog corrente (le celle con manual_override sono saltate).
 */
export async function autoPromoteRoutingMatrixNow(): Promise<{
  ok?: boolean;
  updated?: number;
  skipped_manual?: number;
  no_candidates?: number;
  error?: string;
}> {
  return fetchJson(`${API_BASE}/api/admin/routing-matrix/auto-promote-now`, { method: "POST" });
}
