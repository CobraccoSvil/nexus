// Client API per il Sudo Manager admin (ADR 0017 Livello 1).

import { API_BASE, fetchJson } from "./_shared";

export interface SudoManagerStatus {
  enabled: boolean;
  runner_installed: boolean;
  runner_path: string;
  sudoers_installed: boolean;
  purposes_count: number;
  audit_recent_count: number;
}

export interface SudoPurpose {
  id: string;
  name: string;
  description: string;
  command_template: string;
  requires_confirm: boolean;
  enabled: boolean;
  category: string;
  created_by: string;
  created_at: string | null;
  updated_at: string | null;
}

export interface SudoAuditEntry {
  id: string;
  purpose_name: string;
  full_command: string;
  requested_by_service: string | null;
  exit_code: number | null;
  duration_ms: number | null;
  executed_at: string | null;
}

export interface SudoExecuteResult {
  ok: boolean;
  purpose: string;
  exit_code: number;
  duration_ms: number;
  stdout: string;
  stderr: string;
}

export async function getSudoManagerStatus(): Promise<SudoManagerStatus> {
  return fetchJson(`${API_BASE}/api/admin/sudo/status`);
}

export async function listSudoPurposes(): Promise<{
  items: SudoPurpose[];
  total: number;
}> {
  return fetchJson(`${API_BASE}/api/admin/sudo/purposes`);
}

export async function createSudoPurpose(body: {
  name: string;
  description: string;
  command_template: string;
  requires_confirm?: boolean;
  category?: string;
}): Promise<{ ok: boolean; id: string }> {
  return fetchJson(`${API_BASE}/api/admin/sudo/purposes`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function patchSudoPurpose(
  id: string,
  body: Partial<{
    description: string;
    command_template: string;
    requires_confirm: boolean;
    enabled: boolean;
    category: string;
  }>,
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/sudo/purposes/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export async function deleteSudoPurpose(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/sudo/purposes/${id}`, {
    method: "DELETE",
  });
}

export async function executeSudoPurpose(purpose: string): Promise<SudoExecuteResult> {
  return fetchJson(`${API_BASE}/api/admin/sudo/execute`, {
    method: "POST",
    body: JSON.stringify({ purpose }),
  });
}

export async function listSudoAudit(params?: {
  limit?: number;
  purpose?: string;
}): Promise<{ items: SudoAuditEntry[]; total: number }> {
  const qs = new URLSearchParams();
  if (params?.limit) qs.set("limit", String(params.limit));
  if (params?.purpose) qs.set("purpose", params.purpose);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return fetchJson(`${API_BASE}/api/admin/sudo/audit${suffix}`);
}
