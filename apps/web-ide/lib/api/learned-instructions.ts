// Client API per le learned instructions (mig 0412): regole durature di
// progetto distillate dall'esperienza. Le chiamate passano dalle route proxy
// Next.js (app/api/admin/learned-instructions/*) verso mcp-core (:4000).
import { API_BASE, fetchJson } from "./_shared";

export type LearnedStatus = "proposed" | "active" | "rejected" | "retired";

export interface LearnedRule {
  id: string;
  category: string;
  ruleText: string;
  rationale: string | null;
  status: LearnedStatus;
  confidence: number;
  occurrences: number;
  manuallyEdited: boolean;
  createdAt: string;
  updatedAt: string;
}

export async function listLearnedInstructions(
  projectId: string,
  status?: string,
): Promise<{ data: LearnedRule[]; total: number }> {
  const qs = new URLSearchParams({ project_id: projectId });
  if (status && status !== "all") qs.set("status", status);
  return fetchJson(`${API_BASE}/api/admin/learned-instructions?${qs.toString()}`);
}

export async function patchLearnedInstruction(
  id: string,
  body: { status?: LearnedStatus; rule_text?: string; category?: string },
): Promise<{ id: string; status: string }> {
  return fetchJson(`${API_BASE}/api/admin/learned-instructions/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export async function distillLearnedInstructions(
  projectId: string,
): Promise<{ ok: boolean; applied?: number; error?: string }> {
  return fetchJson(`${API_BASE}/api/admin/learned-instructions/distill`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId }),
  });
}
