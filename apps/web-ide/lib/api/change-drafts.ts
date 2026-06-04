import { fetchJson } from "./_shared";

// ── Change drafts (ChangeDrafter proposte di modifica) ─────────────────

export interface ChangeDraftSummary {
  id: string;
  project_id: string | null;
  trigger_kind: string;
  summary: string;
  status: string;
  created_at: string;
  updated_at: string;
  related_commit_sha: string | null;
}

export interface ChangeDraftDetail extends ChangeDraftSummary {
  draft: {
    razionale?: string;
    impact_analysis?: {
      files_to_modify?: string[];
      files_potentially_affected?: string[];
      breaking_changes?: boolean;
      migration_required?: boolean;
      tests_to_update?: string[];
    };
    diff_proposto?: string;
    verification_steps?: string[];
    alternative_considerate?: Array<{ opzione: string; scartata_perche: string }>;
    doc_da_aggiornare?: string[];
  };
}

export async function listChangeDrafts(params?: {
  status?: string;
  project_id?: string;
  limit?: number;
}): Promise<{ items: ChangeDraftSummary[] }> {
  const sp = new URLSearchParams();
  if (params?.status) sp.set("status", params.status);
  if (params?.project_id) sp.set("project_id", params.project_id);
  if (params?.limit != null) sp.set("limit", String(params.limit));
  const qs = sp.toString();
  return fetchJson(`/api/change-drafts${qs ? `?${qs}` : ""}`);
}

export async function getChangeDraft(id: string): Promise<ChangeDraftDetail> {
  return fetchJson(`/api/change-drafts/${id}`);
}

export async function approveChangeDraft(id: string): Promise<{ id: string; status: string }> {
  return fetchJson(`/api/change-drafts/${id}/approve`, { method: "POST", body: "{}" });
}

export async function rejectChangeDraft(id: string): Promise<{ id: string; status: string }> {
  return fetchJson(`/api/change-drafts/${id}/reject`, { method: "POST", body: "{}" });
}

export async function createChangeDraft(body: {
  project_id?: string;
  trigger_kind: string;
  summary: string;
  draft: Record<string, unknown>;
}): Promise<{ id: string; status: string }> {
  return fetchJson(`/api/change-drafts`, { method: "POST", body: JSON.stringify(body) });
}
