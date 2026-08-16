import { API_BASE, fetchJson } from "./_shared";

export interface AdminSettingEntry {
  key: string;
  value: string;
  category: string;
  description: string;
  is_secret: boolean;
  has_value: boolean;
  updated_at: string;
}

export async function listAdminSettings(): Promise<{ settings: AdminSettingEntry[] }> {
  return fetchJson(`${API_BASE}/api/admin/settings`);
}

export async function listAdminSettingsByCategory(
  category: string,
): Promise<{ settings: AdminSettingEntry[] }> {
  return fetchJson(
    `${API_BASE}/api/admin/settings-by-category/${encodeURIComponent(category)}`,
  );
}

/** L'unico esito di SUCCESSO di una scrittura su settings: la chiave esisteva
 *  ed e' stata aggiornata. Il fallimento non e' un valore di questa unione — il
 *  backend lo dice con lo status HTTP e `fetchJson` solleva `ApiError` (regola M
 *  — lo status e' il segnale, il messaggio serve solo per il display): 404 se la
 *  chiave non esiste, 500 se il DB rifiuta la scrittura.
 *
 *  Il PUT aggiorna, non crea: una chiave nuova si dichiara alla fonte (una
 *  migrazione per i default, `plugins::integrate::publish` per i secret dei
 *  plugin), dove categoria e `is_secret` sono veri. Prima il backend ripiegava
 *  su un INSERT in categoria 'custom' e rispondeva `created`: un refuso nel nome
 *  creava una riga nuova invece di dare errore, e la pagina diceva "salvato" a
 *  una scrittura senza effetto — il sistema legge la chiave giusta, mai quella
 *  col refuso. */
export type AdminSettingUpdateStatus = "ok";

export interface AdminSettingUpdateResult {
  status: AdminSettingUpdateStatus;
  key: string;
}

export async function updateAdminSetting(
  key: string,
  value: string,
): Promise<AdminSettingUpdateResult> {
  return fetchJson(`${API_BASE}/api/admin/setting/${key}`, {
    method: "PUT",
    body: JSON.stringify({ value }),
  });
}

// --- Admin Routing: Purpose models ---
/** La risoluzione LIVE di un purpose: chi risponde ADESSO, e perche'.
 *  `rationale` e' il segnale strutturato del servizio (regola M):
 *  `tier=medium:auto` | `tier=medium:degraded_to=light` | `...:upgraded_to=high`. */
export interface ResolvedPurposeModel {
  provider: string;
  model: string;
  rationale: string;
}

export interface PurposeModelEntry {
  purpose: string;
  /** Fascia della selezione dinamica dal catalog (tier-only, mig 0723: il pin
   *  statico provider/model_id non esiste piu'). `null` = riga storica senza
   *  tier: non risolvibile. */
  notes?: string | null;
  tier?: string | null;
  required_capability?: string | null;
  requires_tool_use?: boolean;
  updated_at: string;
  /** Cosa risolve davvero il resolver ADESSO. `null` = non risolvibile ora
   *  (catalog, gate di qualificazione o cooldown) oppure tier assente. */
  resolved?: ResolvedPurposeModel | null;
}

export async function listAdminPurposeModels(): Promise<{ items: PurposeModelEntry[] }> {
  return fetchJson(`${API_BASE}/api/admin/routing/purpose-models`);
}

export async function updateAdminPurposeModel(
  purpose: string,
  body: {
    /** Obbligatorio dalla mig 0723: senza pin statico un purpose senza tier
     *  non risolve nulla, e il pannello non deve poter produrre quello stato. */
    tier: string;
    notes?: string | null;
    required_capability?: string | null;
    requires_tool_use?: boolean;
  },
): Promise<{ status: string; purpose: string }> {
  return fetchJson(`${API_BASE}/api/admin/routing/purpose-model/${encodeURIComponent(purpose)}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

export async function resolveInternalPurposeModel(
  purpose: string,
): Promise<{ purpose: string; provider: string; model: string; rationale: string }> {
  return fetchJson(`${API_BASE}/api/internal/routing/purpose?purpose=${encodeURIComponent(purpose)}`);
}
