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

/** Esiti di SUCCESSO di una scrittura su settings. Il fallimento non e' un
 *  valore di questa unione: il backend lo dice con lo status HTTP (500) e
 *  `fetchJson` solleva `ApiError` (regola M — lo status e' il segnale, il
 *  messaggio serve solo per il display).
 *
 *  `created` e' un successo per il DB ma un campanello per l'admin: la chiave
 *  non esisteva ed e' stata creata dal PUT, in categoria 'custom' e con
 *  descrizione vuota, invece di essere rifiutata. Chi scrive una chiave che si
 *  aspetta gia' seedata da una migrazione dovrebbe trattarlo come anomalia: il
 *  caso tipico e' un refuso nel nome, che cosi' produce una riga nuova al posto
 *  di un errore. (La riga resta visibile in UI, sotto la categoria 'custom':
 *  la sidebar deriva dai dati, vedi `list_categories` e `buildList`.) */
export type AdminSettingUpdateStatus = "ok" | "created";

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
export interface PurposeModelEntry {
  purpose: string;
  provider: string;
  model_id: string;
  notes?: string | null;
  tier?: string | null;
  required_capability?: string | null;
  requires_tool_use?: boolean;
  updated_at: string;
}

export async function listAdminPurposeModels(): Promise<{ items: PurposeModelEntry[] }> {
  return fetchJson(`${API_BASE}/api/admin/routing/purpose-models`);
}

export async function updateAdminPurposeModel(
  purpose: string,
  body: {
    provider: string;
    model_id: string;
    notes?: string | null;
    tier?: string | null;
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
