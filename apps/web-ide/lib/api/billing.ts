import { API_BASE, fetchJson } from "./_shared";

// --- Billing ---

export interface BillingPrice {
  id: string;
  provider: string;
  model: string;
  input_cost_per_million_tokens: number;
  output_cost_per_million_tokens: number;
  currency: string;
  effective_from: string;
  effective_to?: string;
  is_enabled: boolean;
  updated_at: string;
}

export interface BillingQuota {
  id: string;
  scope_type: "user" | "project" | "user_project";
  user_id?: string;
  project_id?: string;
  token_limit?: number;
  cost_limit?: number;
  currency?: string;
  valid_from: string;
  valid_to: string;
  is_enabled: boolean;
  note: string;
  updated_at: string;
}

interface BillingUsageSummary {
  total_tokens: number;
  total_cost: number;
  total_runs: number;
}

interface BillingUsageBreakdownItem {
  provider: string;
  model: string;
  total_tokens: number;
  total_cost: number;
  runs: number;
}

export interface BillingUsageReport {
  date_from: string;
  date_to: string;
  summary: BillingUsageSummary;
  breakdown: BillingUsageBreakdownItem[];
}

export async function listBillingPrices(): Promise<{ prices: BillingPrice[] }> {
  return fetchJson(`${API_BASE}/api/admin/billing/prices`);
}

export async function createBillingPrice(payload: {
  provider: string;
  model: string;
  input_cost_per_million_tokens: number;
  output_cost_per_million_tokens: number;
  currency?: string;
  effective_from?: string;
  effective_to?: string;
  is_enabled?: boolean;
}): Promise<{ id: string; status: string }> {
  return fetchJson(`${API_BASE}/api/admin/billing/prices`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function listBillingQuotas(): Promise<{ quotas: BillingQuota[] }> {
  return fetchJson(`${API_BASE}/api/admin/billing/quotas`);
}

export async function createBillingQuota(payload: {
  scope_type: "user" | "project" | "user_project";
  user_id?: string;
  project_id?: string;
  token_limit?: number;
  cost_limit?: number;
  currency?: string;
  valid_from: string;
  valid_to: string;
  is_enabled?: boolean;
  note?: string;
}): Promise<{ id: string; status: string }> {
  return fetchJson(`${API_BASE}/api/admin/billing/quotas`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function getSessionUsage(sessionId: string): Promise<{
  totalTokens: number;
  totalCostUsd: number;
  breakdown: Array<{ model: string; tokens: number; costUsd: number }>;
}> {
  const url = new URL(`${API_BASE}/api/billing/session-usage`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("session_id", sessionId);
  const res = await fetchJson<{
    total_tokens: number;
    total_cost_usd: number;
    breakdown: Array<{ model: string; tokens: number; cost_usd: number }>;
  }>(url.toString());
  return {
    totalTokens: res.total_tokens,
    totalCostUsd: res.total_cost_usd,
    breakdown: (res.breakdown ?? []).map((b) => ({
      model: b.model,
      tokens: b.tokens,
      costUsd: b.cost_usd,
    })),
  };
}

export async function getAdminBillingUsage(params?: {
  date_from?: string;
  date_to?: string;
  user_id?: string;
  project_id?: string;
  provider?: string;
  model?: string;
  status?: string;
}): Promise<BillingUsageReport> {
  const url = new URL(`${API_BASE}/api/admin/billing/usage`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  if (params) {
    for (const [key, value] of Object.entries(params)) {
      if (value && value.trim().length > 0) {
        url.searchParams.set(key, value);
      }
    }
  }
  return fetchJson(url.toString());
}

export interface AdminFeedbackItem {
  id: string;
  projectId: string;
  sessionId: string;
  messageId: string;
  userId: string;
  userEmail?: string;
  intent?: string;
  provider?: string;
  model?: string;
  comment: string;
  status: string;
  reviewNote?: string;
  createdAt: string;
}

export interface ProjectLearningConfig {
  enabled: boolean;
  promptCorrectionsEnabled: boolean;
  autoApplyMaxChangesPerDay: number;
  feedbackThreshold: number;
  feedbackWindowDays: number;
  minConfidence: number;
  rollbackWindowHours: number;
}

export interface VectorCompactionRun {
  id: string;
  projectId?: string;
  triggerType: string;
  status: string;
  beforeCount: number;
  afterCount: number;
  dedupCount: number;
  deletedCount: number;
  qdrantDeletedCount: number;
  details: Record<string, unknown>;
  requestedBy?: string;
  startedAt: string;
  finishedAt?: string;
}

export async function getAdminFeedbackErrors(): Promise<{ feedback: AdminFeedbackItem[] }> {
  return fetchJson(`${API_BASE}/api/admin/feedback/errors`);
}

export async function reviewAdminFeedbackError(
  feedbackId: string,
  status: "open" | "reviewed" | "resolved" | "rejected",
  reviewNote?: string,
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/feedback/${feedbackId}/review`, {
    method: "POST",
    body: JSON.stringify({
      status,
      reviewNote,
    }),
  });
}

export async function retrainProjectRouting(
  projectId: string,
  intent?: string,
): Promise<{ ok: boolean; decision: Record<string, unknown> }> {
  return fetchJson(`${API_BASE}/api/admin/learning/projects/${projectId}/retrain-routing`, {
    method: "POST",
    body: JSON.stringify({ intent }),
  });
}

export async function getProjectLearningConfig(
  projectId: string,
): Promise<{ projectId: string; config: ProjectLearningConfig }> {
  return fetchJson(`${API_BASE}/api/admin/learning/projects/${projectId}/config`);
}

export async function updateProjectLearningConfig(
  projectId: string,
  config: Partial<ProjectLearningConfig>,
): Promise<{ ok: boolean; config: ProjectLearningConfig }> {
  return fetchJson(`${API_BASE}/api/admin/learning/projects/${projectId}/config`, {
    method: "PUT",
    body: JSON.stringify({
      enabled: config.enabled,
      promptCorrectionsEnabled: config.promptCorrectionsEnabled,
      autoApplyMaxChangesPerDay: config.autoApplyMaxChangesPerDay,
      feedbackThreshold: config.feedbackThreshold,
      feedbackWindowDays: config.feedbackWindowDays,
      minConfidence: config.minConfidence,
      rollbackWindowHours: config.rollbackWindowHours,
    }),
  });
}

export async function runVectorCompaction(
  projectId?: string,
): Promise<{ ok: boolean; summary: Record<string, unknown> }> {
  return fetchJson(`${API_BASE}/api/admin/vector/compact`, {
    method: "POST",
    body: JSON.stringify({ projectId }),
  });
}

export async function getVectorCompactionRuns(
  limit = 50,
): Promise<{ runs: VectorCompactionRun[] }> {
  const url = new URL(`${API_BASE}/api/admin/vector/compact/runs`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("limit", String(limit));
  return fetchJson(url.toString());
}
