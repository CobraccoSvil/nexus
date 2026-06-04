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

export async function updateAdminSetting(
  key: string,
  value: string,
): Promise<{ status: string; key: string }> {
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
  updated_at: string;
}

export async function listAdminPurposeModels(): Promise<{ items: PurposeModelEntry[] }> {
  return fetchJson(`${API_BASE}/api/admin/routing/purpose-models`);
}

export async function updateAdminPurposeModel(
  purpose: string,
  body: { provider: string; model_id: string; notes?: string | null },
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
