import { API_BASE, fetchJson } from "./_shared";

// ── Model Catalog ──────────────────────────────────────────────────────────

/**
 * Specchio ESATTO del wire di `/api/models` (punto unico, regola L): il
 * serializzatore autoritativo e' `crates/mcp-core/src/models.rs::ModelCatalogEntry`,
 * annotato `#[serde(rename_all = "camelCase")]` -> i campi arrivano in camelCase
 * con il suffisso `Tokens` sui costi (`inputCostPerMillionTokens`).
 *
 * Ogni consumatore di `/api/models` deve usare QUESTO tipo: le copie locali
 * divergono in silenzio dal wire e i campi mancanti diventano `undefined`, che
 * un `?? 0` a valle trasforma in "costo zero" invece che in un errore visibile
 * (regola G: niente magic fallback). E' esattamente il difetto che azzerava il
 * footer costo-per-provider del nastro attivita'.
 */
export type PerformanceTier = "light" | "medium" | "high" | "heavy" | "frontier";

/** Chi ha stabilito il tier (`ai_price_catalog.tier_source`, mig 0608).
 *  `null` = nessuna fonte si e' espressa: il valore c'e' ma non si sa da dove
 *  venga (un fossile), e indice o batteria possono rimpiazzarlo. */
export type TierSource = "synced" | "measured" | "manual";

export interface ModelCatalogEntry {
  provider: string;
  model: string;
  displayName: string;
  inputCostPerMillionTokens: number;
  outputCostPerMillionTokens: number;
  /** Tariffa dei token letti da cache (`ai_price_catalog`, mig 0130/0403).
   *  `null` = il catalog non la conosce per questo modello: la UI mostra il
   *  costo senza sconto cache invece di inventarsi un rapporto. */
  cacheReadCostPerMillionTokens: number | null;
  currency: string;
  /** `null` = tier ignoto (mig 0599: la colonna e' nullable, e NULL significa
   *  "nessuna fonte lo ha stabilito" — non "medium"). */
  performanceTier: PerformanceTier | null;
  tierSource: TierSource | null;
  /** Indice della classificazione esterna (Artificial Analysis via OpenRouter):
   *  il numero su cui si fonda il tier `synced`. `null` = modello non coperto. */
  agenticIndex: number | null;
  /** Stato della batteria di qualificazione. Col gate acceso solo `qualified`
   *  entra nel routing agentico. */
  qualificationState: string | null;
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

/** Alias storico: `/api/models` ha UN solo wire, quindi un solo tipo (regola L).
 *  Mantenuto per i call site che lo importano con questo nome. */
export type ModelCatalogItem = ModelCatalogEntry;

export async function listModelCatalog(): Promise<{ models: ModelCatalogEntry[] }> {
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

/**
 * Curatela del tier: decide la fascia di un modello (`tier_source='manual'`,
 * che vince su indice e batteria). `tier: null` RIMUOVE la curatela senza
 * azzerare il tier — il valore resta e le fonti automatiche tornano a poterlo
 * correggere.
 */
export async function setModelTier(
  provider: string,
  model: string,
  tier: PerformanceTier | null,
): Promise<{ ok: boolean; provider: string; model: string; tier: string | null; changed: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/provider-models/tier`, {
    method: "PUT",
    body: JSON.stringify({ provider, model, tier }),
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
