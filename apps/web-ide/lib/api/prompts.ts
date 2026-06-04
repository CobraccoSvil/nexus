import { API_BASE, adminServiceUrl, fetchJson } from "./_shared";

// --- Prompt Templates ---
export interface PromptTemplate {
  key: string;
  category: string;
  title: string;
  content: string;
  version: number;
  is_active: boolean;
  usage_context?: string;
  updated_by: string;
  updated_at: string;
  /** Schema del prompt: 'plain' (legacy) o 'xml' (v2 strutturato). */
  schema_type?: "plain" | "xml";
  /** Placeholder dichiarati dal prompt (es. ["lang_hint","type_hint"]). */
  placeholder_vars?: string[];
  /** Variante sperimentale generata dal PromptOptimizerWorker. */
  experimental?: boolean;
}

export interface PromptPreviewResponse {
  key: string;
  schema_type: string;
  rendered: string;
  unresolved_placeholders: string[];
}

export async function previewPromptTemplate(
  key: string,
  body: { intent?: string; repo_lang?: string; repo_summary?: string },
): Promise<PromptPreviewResponse> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}/preview`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export interface PromptTemplateHistory {
  id: string;
  version: number;
  content: string;
  changed_by: string;
  changed_at: string;
  change_note?: string;
}

export async function listPromptTemplates(): Promise<PromptTemplate[]> {
  return fetchJson(`${API_BASE}/api/prompt-templates`);
}

export async function getPromptTemplate(key: string): Promise<{ template: PromptTemplate; history: PromptTemplateHistory[] }> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}`);
}

export async function updatePromptTemplate(key: string, content: string, changeNote?: string): Promise<PromptTemplate> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}`, {
    method: "PUT",
    body: JSON.stringify({ content, change_note: changeNote }),
  });
}

export async function disablePromptTemplate(key: string): Promise<{ status: string }> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}/disable`, { method: "POST" });
}

export async function enablePromptTemplate(key: string): Promise<{ status: string }> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}/enable`, { method: "POST" });
}

export interface AiSuggestResponse {
  suggestion: string;
  provider: string;
  model: string;
  suggested_tools?: PromptMcpTool[];  // STEP 8: tool suggeriti automaticamente
}

export async function aiSuggestPromptTemplate(key: string, instruction: string): Promise<{ suggestion: string; suggested_tools?: PromptMcpTool[] }> {
  return fetchJson(`${API_BASE}/api/prompt-templates/${encodeURIComponent(key)}/ai-suggest`, {
    method: "POST",
    body: JSON.stringify({ instruction }),
  });
}

// --- MCP Tools for Prompts ---

export interface PromptMcpTool {
  tool_name: string;
  tool_server: string;
  usage_context?: string;
  confidence?: number;  // STEP 7: confidence score (0.0 - 1.0) per semantic selection
  method?: string;      // STEP 7: metodo di selezione (semantic, keyword, lazy_default)
}

export interface PromptToolsResponse {
  assigned_tools: PromptMcpTool[];
  suggested_tools: PromptMcpTool[];
  available_tools: PromptMcpTool[];
}

export interface AvailableMcpTool {
  name: string;
  server: string;
  description?: string;
  input_schema?: Record<string, unknown>;
}

export async function getPromptTools(key: string): Promise<PromptToolsResponse> {
  return fetchJson(`${API_BASE}/api/admin/prompt-templates/${encodeURIComponent(key)}/tools`);
}

export async function updatePromptTools(key: string, tools: PromptMcpTool[]): Promise<void> {
  return fetchJson(`${API_BASE}/api/admin/prompt-templates/${encodeURIComponent(key)}/tools`, {
    method: "PUT",
    body: JSON.stringify({ assigned_tools: tools }),
  });
}

export async function batchAssignAllTools(): Promise<{ processed: number; assigned: number; skipped: number; errors: number }> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 600_000); // 10 min
  try {
    return await fetchJson(`${API_BASE}/api/admin/prompt-templates/batch-assign-tools`, {
      method: "POST",
      signal: controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
}

export async function getAvailableMcpTools(): Promise<AvailableMcpTool[]> {
  return fetchJson(`${API_BASE}/api/admin/available-mcp-tools`);
}

// ─── Esperimenti A/B prompt (Fase 3) ─────────────────────────────────────────

export interface PromptExperiment {
  id: string;
  prompt_key: string;
  baseline_version: number;
  variant_version: number;
  traffic_pct: number;
  status: "running" | "promoted" | "discarded" | "rolled_back";
  started_at: string;
  ended_at?: string;
  baseline_success_rate?: number;
  variant_success_rate?: number;
  baseline_reflection_avg?: number;
  variant_reflection_avg?: number;
  p_value?: number;
  decision_reason?: string;
  auto_promote_enabled: boolean;
  baseline_content?: string;
  variant_content?: string;
  baseline_stats?: { runs: number; avg_score: number; min_score: number; max_score: number };
  variant_stats?: { runs: number; avg_score: number; min_score: number; max_score: number };
}

export interface PromptDashboardEntry {
  prompt_key: string;
  prompt_version: number;
  schema_type: string;
  experimental: boolean;
  avg_reflection_score?: number;
  reflection_runs: number;
  feedback_positive_rate?: number;
  feedback_count: number;
}

export interface PromptDashboardData {
  prompts: PromptDashboardEntry[];
  active_experiments: PromptExperiment[];
  global_reflection_avg_7d?: number;
  total_prompts: number;
  running_experiments: number;
}

export async function listPromptExperiments(): Promise<{ experiments: PromptExperiment[]; total: number }> {
  return fetchJson(`${adminServiceUrl("/prompt-experiments")}`);
}

export async function getPromptExperiment(id: string): Promise<PromptExperiment> {
  return fetchJson(`${adminServiceUrl(`/prompt-experiments/${encodeURIComponent(id)}`)}`);
}

export async function forcePromoteExperiment(id: string): Promise<{ ok: boolean; decision: string }> {
  return fetchJson(`${adminServiceUrl(`/prompt-experiments/${encodeURIComponent(id)}/promote`)}`, {
    method: "POST",
  });
}

export async function forceDiscardExperiment(id: string): Promise<{ ok: boolean; decision: string }> {
  return fetchJson(`${adminServiceUrl(`/prompt-experiments/${encodeURIComponent(id)}/discard`)}`, {
    method: "POST",
  });
}

export async function getPromptDashboard(): Promise<PromptDashboardData> {
  return fetchJson(`${adminServiceUrl("/prompt-dashboard")}`);
}

// ── Direttive condivise agenti ──────────────────────────────────────────

export interface SharedDirective {
  key: string;
  content: string;
  scope: "agent" | "system" | "all";
  priority: number;
  isActive: boolean;
  description: string | null;
  createdAt: string;
  updatedAt: string;
}

export async function listSharedDirectives(): Promise<{
  directives: SharedDirective[];
  total: number;
}> {
  return fetchJson(`${adminServiceUrl("/shared-directives")}`);
}

export async function getSharedDirective(key: string): Promise<SharedDirective> {
  return fetchJson(
    `${adminServiceUrl(`/shared-directives/${encodeURIComponent(key)}`)}`,
  );
}

export async function createSharedDirective(data: {
  key: string;
  content: string;
  scope?: string;
  priority?: number;
  description?: string;
}): Promise<SharedDirective> {
  return fetchJson(`${adminServiceUrl("/shared-directives")}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export async function updateSharedDirective(
  key: string,
  data: {
    content?: string;
    scope?: string;
    priority?: number;
    isActive?: boolean;
    description?: string;
  },
): Promise<SharedDirective> {
  return fetchJson(
    `${adminServiceUrl(`/shared-directives/${encodeURIComponent(key)}`)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(data),
    },
  );
}

export async function toggleSharedDirective(key: string): Promise<SharedDirective> {
  return fetchJson(
    `${adminServiceUrl(`/shared-directives/${encodeURIComponent(key)}/toggle`)}`,
    { method: "POST" },
  );
}

export async function deleteSharedDirective(key: string): Promise<{ ok: boolean }> {
  return fetchJson(
    `${adminServiceUrl(`/shared-directives/${encodeURIComponent(key)}`)}`,
    { method: "DELETE" },
  );
}
