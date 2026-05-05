const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";
// Proxy tramite Next.js /api/neural/* → brain:8001 (evita CORS e NEXT_PUBLIC_* baked)
const NEURAL_BASE = "/api/neural";

export function getApiBaseUrl(): string {
  return API_BASE;
}

/** Route Next.js che proxyano verso admin-service (:4010) — NON devono puntare a mcp-core (:4000). */
function adminServiceUrl(path: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  if (typeof window !== "undefined") {
    return `/api/admin${p}`;
  }
  // SSR: niente host relativo — proxa via Next sullo stesso origin dev (Web IDE).
  const origin =
    process.env.NEXT_INTERNAL_ORIGIN ||
    process.env.NEXT_PUBLIC_APP_ORIGIN ||
    "http://127.0.0.1:3000";
  return `${origin}/api/admin${p}`;
}

async function fetchJson<T>(url: string, init?: RequestInit, timeoutMs = 30000): Promise<T> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort("timeout"), timeoutMs);
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      credentials: "include",
      headers: { "Content-Type": "application/json", ...init?.headers },
      signal: init?.signal ?? controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
  if (res.status === 401 && typeof window !== "undefined" && !window.location.pathname.startsWith("/login")) {
    window.location.href = "/login";
    throw new Error("Sessione scaduta");
  }
  if (!res.ok) {
    let details = "";
    try {
      const payload = await res.json();
      const rawError =
        typeof payload?.error === "string"
          ? payload.error
          : typeof payload?.message === "string"
            ? payload.message
            : "";
      if (rawError) {
        const firstLine = rawError
          .split("\n")
          .map((line: string) => line.trim())
          .find((line: string) => line.length > 0);
        const compact = (firstLine ?? rawError).replace(/\s+/g, " ").trim();
        const reduced = compact.length > 600 ? `${compact.slice(0, 600)}...` : compact;
        details = ` - ${reduced}`;
      }
    } catch {
      // ignore body parse errors and keep generic status details
    }
    throw new Error(`API error ${res.status}: ${res.statusText}${details}`);
  }
  return res.json();
}

async function fetchJsonNoAuth<T>(url: string, init?: RequestInit, timeoutMs = 5000): Promise<T> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);
  let res: Response;
  try {
    res = await fetch(url, {
      ...init,
      headers: { "Content-Type": "application/json", ...init?.headers },
      signal: init?.signal ?? controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
  if (!res.ok) throw new Error(`API error ${res.status}: ${res.statusText}`);
  return res.json();
}


// --- MCP Core (Rust :4000) ---

export interface HealthResponse {
  status: string;
  components: {
    database: boolean;
    redis: boolean;
    neural_core: boolean;
    /** gRPC ToolRunner :50071 — se false, l'AI non può eseguire tool MCP */
    tools_grpc?: boolean;
  };
}

export interface DashboardResponse {
  total_runs: number;
  tokens_consumed: number;
  tokens_saved: number;
  quality_findings: number;
  active_jobs: number;
  recent_runs: Array<{
    id: string;
    status: string;
    created_at: string;
  }>;
}

export interface ChatResponse {
  content: string;
  provider: string;
  model: string;
  tokens_used: number;
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  total_cost?: number;
  currency?: string;
  quota_status?: string;
}

export interface ChatSessionSummary {
  id: string;
  projectId: string;
  title: string;
  status: string;
  messageCount: number;
  lastMessageAt?: string;
  lastMessagePreview?: string;
  createdAt: string;
  updatedAt: string;
}

export interface ChatMessage {
  id: string;
  sessionId: string;
  projectId: string;
  role: "user" | "assistant";
  content: string;
  requestMessageId?: string;
  deletedAt?: string;
  createdAt: string;
  provider?: string;
  model?: string;
  intent?: string;
  runId?: string;
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
  totalCost?: number;
  currency?: string;
  automationMode?: "study" | "confirm" | "automatic";
  resendOfMessageId?: string;
}

export interface ChatAttachment {
  name: string;
  mimeType: string;
  sizeBytes: number;
  textContent: string;
  base64Content?: string;
}

export interface SendChatMessageOptions {
  profileId?: string;
  activeFiles?: string[];
  providerOverride?: string;
  modelOverride?: string;
  automationMode?: "study" | "confirm" | "automatic";
  supervisorMode?: "none" | "anomaly" | "interleaved" | "continuous";
  attachments?: ChatAttachment[];
}

export interface FeedbackErrorResponse {
  ok: boolean;
  feedbackId: string;
  correctionId: string;
  deduplicatedCount: number;
  learning: Record<string, unknown>;
}

export interface UserProjectSummary {
  id: string;
  name: string;
  slug: string;
  ownerUserId: string;
  currentUserRole: string;
  canWrite: boolean;
  canManageGit: boolean;
  isShared: boolean;
  visibility: string;
  workspaceId?: string;
  rootPath?: string;
  isGitRepo: boolean;
  currentBranch?: string;
  lastOpenedAt?: string;
  analyzedAt?: string | null;
  isAnalyzed: boolean;
  nexusReady: boolean;
  defaultProfileId?: string | null;
}

export interface UserProjectDetails extends UserProjectSummary {
  repositoryRootPath?: string;
}

export interface WorkspaceTreeNode {
  name: string;
  path: string;
  kind: "directory" | "file";
  hasChildren: boolean;
}

export interface BrowseDirectoryNode {
  name: string;
  path: string;
  hasChildren: boolean;
}

export interface BrowseDirectoriesResponse {
  roots: string[];
  currentPath: string;
  parentPath?: string;
  directories: BrowseDirectoryNode[];
}

export interface ProjectFileBuffer {
  path: string;
  content: string;
}

export type WorkbenchLayoutMode = "ai-center" | "editor-center" | "split-ai-editor";

export interface EditorTabState {
  path: string;
  title: string;
  dirty: boolean;
  pinned: boolean;
  content?: string;
  /** Vista corrente del tab: "source" (Monaco) o "preview" (rendering markdown).
   *  Significativo solo per file .md/.markdown; default "source". */
  viewMode?: "source" | "preview";
}

export interface EditorGroupState {
  id: string;
  tabs: EditorTabState[];
  activePath?: string | null;
}

export interface WorkbenchState {
  layoutMode: WorkbenchLayoutMode;
  primarySidebarVisible: boolean;
  secondarySidebarVisible?: boolean;
  secondarySidebarView?: string;
  layoutControlStyle?: "icon-menu";
  iconButtonsOnly?: boolean;
  bottomPanelVisible: boolean;
  activeSidebarView: string;
  activePanelTab: string;
  leftWidth: number;
  rightWidth: number;
  bottomHeight: number;
  editorGroups: EditorGroupState[];
  ai: {
    activeContextPaths: string[];
  };
  terminal: {
    activeTabId?: string | null;
    tabs: Array<{ id: string; title: string }>;
  };
  chat?: {
    provider: string;
    model: string;
    automationMode: "study" | "confirm" | "automatic";
  };
}

export interface SearchResultItem {
  path: string;
  line: number;
  column: number;
  preview: string;
}

export interface ProblemItem {
  id: string;
  severity: string;
  source: string;
  message: string;
  filePath?: string;
  line?: number;
  column?: number;
  createdAt: string;
}

export interface OutputChannel {
  id: string;
  label: string;
  title?: string;   // tooltip esteso (es. nome unit completo)
  kind?: "service" | "task";
}

export interface OutputEvent {
  id: string;
  channel: string;
  level: string;
  title: string;
  text: string;
  createdAt: string;
}

export interface AgentProcess {
  id: string;
  label: string;
  command: string;
  status: string;
  exitCode: number | null;
  output: string;
  errorOutput: string;
  pid: number | null;
  createdAt: string;
}

export type RunConfigRole = "frontend" | "backend" | "service" | "test" | "tool";

export interface RunConfigItem {
  id: string;
  label: string;
  kind: string;
  command: string;
  role?: RunConfigRole | string | null;
  essential?: boolean;
  group?: string | null;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
}

export interface PortEntry {
  port?: number;
  label?: string;
  state?: string;
  url?: string;
  /** Short-name del servizio systemd a cui appartiene la porta (se rilevato). */
  service?: string | null;
}

export interface PlaywrightRunSummary {
  id: string;
  label: string;
  status: string;
  summary?: string;
  createdAt: string;
}

export interface GitFileChange {
  path: string;
  stagedStatus: string;
  worktreeStatus: string;
  kind: string;
  staged: boolean;
  unstaged: boolean;
  untracked: boolean;
}

export interface GitRepositoryState {
  isGitRepo: boolean;
  currentBranch?: string;
  staged: GitFileChange[];
  unstaged: GitFileChange[];
  untracked: GitFileChange[];
}

export interface GitBranchInfo {
  name: string;
  isCurrent: boolean;
  upstream?: string;
}

export interface GitLogEntry {
  commit: string;
  shortCommit: string;
  author: string;
  date: string;
  subject: string;
  body?: string;
}

export interface GitDiffResponse {
  path: string;
  staged: boolean;
  diff: string;
}

export interface GitHubAccountStatus {
  username?: string | null;
  avatarUrl?: string | null;
  status: "connected" | "upgrade_required" | "reconnect_required" | "not_connected";
  connected: boolean;
  scopes: string[];
  expiresAt?: string | null;
}

export interface GitHubPullRequestSummary {
  number: number;
  htmlUrl: string;
  title: string;
  state: string;
}

export interface GitHubRemoteStatus {
  available: boolean;
  reason: string;
  remoteName?: string;
  remoteUrl?: string;
  owner?: string;
  repo?: string;
  repoFullName?: string;
  branch?: string;
  upstream?: string;
  ahead: number;
  behind: number;
  published: boolean;
  defaultBranch?: string;
  canPushPull: boolean;
  suggestedPrTitle?: string;
  lastCommitTitle?: string;
  pullRequest?: GitHubPullRequestSummary | null;
  apiError?: string;
}

export interface GitHubRepositorySummary {
  id: number;
  name: string;
  fullName: string;
  ownerLogin: string;
  htmlUrl: string;
  cloneUrl: string;
  private: boolean;
  defaultBranch: string;
  updatedAt: string;
}

export interface GitUiPreferences {
  showHunkMap: boolean;
}

export interface TerminalSession {
  sessionId: string;
  token: string;
  workingDirectory: string;
  shell: string;
  expiresAt: number;
}

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

// --- Feedback Assist ---
export async function feedbackAssist(
  messageContent: string,
  description: string,
): Promise<{ suggestion: string }> {
  return fetchJson(`${API_BASE}/api/chat/feedback-assist`, {
    method: "POST",
    body: JSON.stringify({ message_content: messageContent, description }),
  });
}

// --- Project Porting ---
export interface PortDetail {
  id: string;
  table: string;
  projectId: string;
  projectName: string;
  oldPath: string;
  newPath: string;
}

export async function portProjects(
  oldBase: string,
  newBase: string,
  preview: boolean,
): Promise<{ details: PortDetail[]; workspacesUpdated: number; repositoriesUpdated: number; projectsBaseRootUpdated: boolean; error?: string }> {
  return fetchJson(`${API_BASE}/api/admin/port-projects`, {
    method: "POST",
    body: JSON.stringify({ old_base: oldBase, new_base: newBase, preview }),
  });
}

export async function getHealth(): Promise<HealthResponse> {
  return fetchJson(`${API_BASE}/api/health`);
}

export async function getDashboard(): Promise<DashboardResponse> {
  return fetchJson(`${API_BASE}/api/dashboard`);
}

export async function generateSystemPrompt(
  profileName: string,
  description?: string,
  provider?: string,
  model?: string,
): Promise<{ text: string }> {
  return fetchJson(`${API_BASE}/api/ai/generate-prompt`, {
    method: "POST",
    body: JSON.stringify({ profile_name: profileName, description, provider, model }),
  });
}

export async function sendChat(
  projectId: string,
  profileId: string,
  message: string,
): Promise<ChatResponse> {
  return fetchJson(`${API_BASE}/api/chat`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
  });
}

export async function getChatSessions(projectId: string): Promise<{ sessions: ChatSessionSummary[] }> {
  const url = new URL(`${API_BASE}/api/chat/sessions`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("projectId", projectId);
  return fetchJson(url.toString());
}

export async function createChatSession(
  projectId: string,
  title?: string,
): Promise<{ session: { id: string; projectId: string; title: string; status: string } }> {
  return fetchJson(`${API_BASE}/api/chat/sessions`, {
    method: "POST",
    body: JSON.stringify({ projectId, title }),
  });
}

export async function renameChatSession(
  sessionId: string,
  title: string,
): Promise<{ ok: boolean; title: string }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}`, {
    method: "PATCH",
    body: JSON.stringify({ title }),
  });
}

export async function deleteChatSession(
  sessionId: string,
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}`, {
    method: "DELETE",
  });
}

export interface CompactSessionResponse {
  ok: boolean;
  summary: string;
  pointId: string;
}

export async function compactChatSession(
  sessionId: string,
): Promise<CompactSessionResponse> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/compact`, {
    method: "POST",
  });
}

export interface ProjectMemory {
  id: string;
  sessionId?: string;
  sessionTitle: string;
  summary: string;
  active: boolean;
  createdAt: string;
}

export async function listProjectMemories(
  projectId: string,
): Promise<{ memories: ProjectMemory[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/memories`);
}

export async function toggleProjectMemory(
  memoryId: string,
): Promise<{ ok: boolean; active: boolean }> {
  return fetchJson(`${API_BASE}/api/memories/${memoryId}/toggle`, {
    method: "PATCH",
  });
}

export async function getChatMessages(
  sessionId: string,
): Promise<{ sessionId: string; projectId: string; messages: ChatMessage[] }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/messages`);
}

export interface AgentStepUsage {
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

export interface AgentStep {
  stepIndex: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  toolResult?: string;
  status: "running" | "completed" | "failed" | "awaiting_confirmation" | "skipped" | "provider_unavailable";
  createdAt: string;
  // Metriche estese
  usage?: AgentStepUsage;
  costUsd?: number;
  latencyMs?: number;
  temperature?: number;
  topP?: number;
}

export interface AITraceToolCall {
  name: string;
  input: Record<string, unknown>;
}

export interface AITraceEvent {
  runId: string;
  iteration: number;
  provider: string;
  model: string;
  messagesSent: number;
  toolsCount: number;
  responseText: string;
  toolCalls: AITraceToolCall[];
  stopReason: string;
  timestamp: string;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
}

export interface AgentPendingAction {
  index: number;
  toolName: string;
  toolInput: Record<string, unknown>;
  description: string;
}

export interface AgentRunUsage {
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

export interface AgentRunInfo {
  runId: string;
  sessionId: string;
  status: "running" | "completed" | "awaiting_confirmation" | "failed" | "timed_out" | "cancelled" | "interrupted";
  automationMode: string;
  provider: string;
  model: string;
  iterationCount: number;
  finalAnswer?: string;
  /** Avviso privacy per provider non-EU/non-locali (DeepSeek, OpenAI, Google, Anthropic). */
  providerPrivacyNotice?: string;
  pendingActions: AgentPendingAction[];
  steps: AgentStep[];
  createdAt: string;
  completedAt?: string;
  // Metriche estese del run complessivo
  usage?: AgentRunUsage;
  totalCostUsd?: number;
  totalLatencyMs?: number;
  cacheHitRate?: number;
  temperature?: number;
  topP?: number;
}

export async function sendChatMessage(
  sessionId: string,
  content: string,
  options: SendChatMessageOptions = {},
): Promise<{ sessionId: string; userMessage: ChatMessage; assistantMessage?: ChatMessage; agentRun?: { runId: string; status: string; provider: string; model: string } }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/messages`, {
    method: "POST",
    body: JSON.stringify({
      content,
      profileId: options.profileId ?? "default",
      activeFiles: options.activeFiles ?? [],
      providerOverride: options.providerOverride,
      modelOverride: options.modelOverride,
      automationMode: options.automationMode ?? "confirm",
      supervisorMode: options.supervisorMode ?? "none",
      attachments: options.attachments ?? [],
    }),
  }, 120000);
}

export interface PrecheckResult {
  ok: boolean;
  correctedText: string | null;
  contextSuggestion: string | null;
  issues: string[];
  reason: string | null;
}

export async function precheckChatMessage(message: string): Promise<PrecheckResult> {
  return fetchJson(`${API_BASE}/api/chat/precheck`, {
    method: "POST",
    body: JSON.stringify({ message }),
  });
}

export async function resendChatMessage(
  messageId: string,
  options: SendChatMessageOptions = {},
): Promise<{ sessionId: string; userMessage?: ChatMessage; assistantMessage?: ChatMessage; agentRun?: { runId: string; status: string; provider: string; model: string } }> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/resend`, {
    method: "POST",
    body: JSON.stringify({
      content: "",
      profileId: options.profileId ?? "default",
      activeFiles: options.activeFiles ?? [],
      providerOverride: options.providerOverride,
      modelOverride: options.modelOverride,
      automationMode: options.automationMode,
      attachments: options.attachments ?? [],
    }),
  }, 120000);
}

export async function deleteChatMessage(
  messageId: string,
): Promise<{ ok: boolean; messageId: string }> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}`, {
    method: "DELETE",
  });
}

export async function feedbackChatMessageError(
  messageId: string,
  comment: string,
): Promise<FeedbackErrorResponse> {
  return fetchJson(`${API_BASE}/api/chat/messages/${messageId}/feedback-error`, {
    method: "POST",
    body: JSON.stringify({ comment }),
  });
}

export async function getMyProjects(): Promise<{ projects: UserProjectSummary[] }> {
  return fetchJson(`${API_BASE}/api/projects/mine`);
}

export interface DeleteProjectResult {
  ok?: boolean;
  deleted?: string;
  rootPath?: string;
  // Returned when there are pending changes and force=false
  hasPendingChanges?: boolean;
  dirtyCount?: number;
  projectName?: string;
}

export async function deleteProject(
  projectId: string,
  force = false,
): Promise<DeleteProjectResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}?force=${force}`, {
    method: "DELETE",
  });
}

export async function registerProject(
  absolutePath: string,
  name?: string,
): Promise<{ project: UserProjectDetails }> {
  return fetchJson(`${API_BASE}/api/projects/register`, {
    method: "POST",
    body: JSON.stringify({ absolute_path: absolutePath, name }),
  });
}

export async function cloneProject(
  url: string,
  name?: string,
): Promise<{ project: UserProjectDetails }> {
  return fetchJson(`${API_BASE}/api/projects/clone`, {
    method: "POST",
    body: JSON.stringify({ url, name }),
  });
}

export async function checkCloneTargetExists(
  repo: string,
): Promise<{ exists: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/projects/clone-target-exists?repo=${encodeURIComponent(repo)}`);
}

export interface ProjectAnalysis {
  projectId: string;
  rootPath: string;
  totalFiles: number;
  filesByExtension: Record<string, number>;
  languages: { language: string; fileCount: number }[];
  frameworks: string[];
  dependencies: Record<string, unknown>;
  git: {
    isGitRepo: boolean;
    branch?: string;
    dirtyFiles?: number;
    remotes?: string[];
  };
  structure: {
    hasReadme: boolean;
    hasGitignore: boolean;
    hasLicense: boolean;
    hasCi: boolean;
  };
  vectorIndex?: {
    status: "indexed" | "partial" | "error" | "skipped";
    collection?: string;
    documents?: number;
    indexedPoints?: number;
    failedPoints?: number;
    error?: string | null;
    updatedAt?: string;
  };
}

export async function analyzeProject(projectId: string): Promise<ProjectAnalysis> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/analyze`, {
    method: "POST",
  });
}

// ── Deep analysis (agent.project.analyzer) ──────────────────────────────────

export interface DeepAnalysisIssue {
  severity: "high" | "medium" | "low";
  title: string;
  files: string[];
  description: string;
  suggested_fix: string;
}

export interface DeepAnalysisService {
  name: string;
  type: string;
  port: number | null;
  start_command: string;
  depends_on: string[];
  config_files: string[];
  /** Modalita' di esecuzione consigliata per lo sviluppo locale. */
  recommended_run_mode?: "native" | "docker" | "either";
  /** Motivazione (1 frase) della modalita' consigliata. */
  run_mode_rationale?: string;
}

export interface DeepAnalysisInsights {
  project_summary: string;
  domain: string;
  architecture: {
    pattern: string;
    description: string;
    primary_languages: string[];
    primary_frameworks: string[];
  };
  services: DeepAnalysisService[];
  config_issues: DeepAnalysisIssue[];
  pre_launch_checks: Array<{ service: string; checks: string[] }>;
  suggested_actions: Array<{ priority: number; title: string; command: string | null; rationale: string }>;
  notes?: string;
}

export interface DeepAnalysisResponse {
  status: "completed" | "partial" | "failed";
  insights: DeepAnalysisInsights | null;
  model_used: string | null;
  duration_ms: number;
  config_files_count: number;
  registered_services_count: number;
  error?: string | null;
}

/** Risposta del POST /deep-analyze (refactor 0102: ora asincrono).
 *  Il server fa insert riga 'running' immediato e lancia il job in background.
 *  Il client poi polla GET /insights ogni 3s finche' status != 'running'. */
export interface DeepAnalysisStartResponse {
  run_id: number;
  status: "running";
  message: string;
}

/** Avvia l'analisi profonda AI (agent.project.analyzer) in background.
 *  Ritorna subito con run_id. Per attendere il completamento usare
 *  `pollProjectInsightsUntilDone(projectId)`. */
export async function analyzeProjectDeep(projectId: string): Promise<DeepAnalysisStartResponse> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/deep-analyze`, {
    method: "POST",
  });
}

export interface ProjectInsightsResponse {
  exists: boolean;
  insights?: DeepAnalysisInsights;
  model_used?: string;
  duration_ms?: number;
  config_files_count?: number;
  status?: string;
  error_message?: string | null;
  created_at?: string;
}

/** Recupera l'ultima analisi profonda salvata per il progetto. */
export async function getProjectInsights(projectId: string): Promise<ProjectInsightsResponse> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/insights`);
}

/** Polla GET /insights ogni `intervalMs` finche' `status != 'running'` o
 *  fino a `maxAttempts`. Risolve quando l'analisi e' completata o fallita.
 *  Default: 3s × 120 attempts = 6 minuti totali (sufficiente per LLM lente). */
export async function pollProjectInsightsUntilDone(
  projectId: string,
  intervalMs: number = 3000,
  maxAttempts: number = 120,
): Promise<ProjectInsightsResponse> {
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const r = await getProjectInsights(projectId);
    if (!r.exists || (r.status && r.status !== "running")) {
      return r;
    }
    await new Promise((res) => setTimeout(res, intervalMs));
  }
  // Timeout: ritorna lo stato corrente anche se ancora 'running'
  return await getProjectInsights(projectId);
}

export async function browseServerDirectories(path?: string): Promise<BrowseDirectoriesResponse> {
  const buildUrl = (base: string) => {
    const url = new URL(`${base}/api/fs/directories`);
    if (path?.trim()) {
      url.searchParams.set("path", path.trim());
    }
    return url.toString();
  };

  try {
    return await fetchJson(buildUrl(API_BASE));
  } catch (error) {
    if (
      error instanceof Error &&
      error.message.includes("API error 404") &&
      typeof window !== "undefined"
    ) {
      const fallbackBase = `${window.location.protocol}//${window.location.hostname}:4000`;
      if (fallbackBase !== API_BASE) {
        return fetchJson(buildUrl(fallbackBase));
      }
    }
    throw error;
  }
}

export async function createServerDirectory(
  parentPath: string,
  name: string,
): Promise<{ ok: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/fs/directories/create`, {
    method: "POST",
    body: JSON.stringify({
      parent_path: parentPath,
      name,
    }),
  });
}

export async function openProject(
  projectId: string,
): Promise<{ project: UserProjectDetails; tree: WorkspaceTreeNode[]; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/open`, { method: "POST" });
}

export async function createTerminalSession(projectId: string): Promise<TerminalSession> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/terminal/session`, {
    method: "POST",
  });
}

export async function setTerminalPresence(
  projectId: string,
  consumerId: string,
  connected: boolean,
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/terminal-commands/presence`, {
    method: "POST",
    body: JSON.stringify({ consumerId, connected }),
  });
}

export async function ackTerminalCommand(
  projectId: string,
  commandId: string,
  payload: {
    consumerId: string;
    delivered: boolean;
    outputPreview?: string;
    error?: string;
  },
): Promise<{ ok: boolean; status: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/terminal-commands/${commandId}/ack`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function stopAgentProcess(
  projectId: string,
  processId: string,
): Promise<{ ok: boolean; message: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/agent-processes/${processId}/stop`, {
    method: "POST",
  });
}

export async function clearFinishedProcesses(
  projectId: string,
): Promise<{ ok: boolean; deleted: number }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/agent-processes/clear-finished`, {
    method: "POST",
  });
}

export async function finishTerminalCommand(
  projectId: string,
  commandId: string,
  payload: {
    consumerId: string;
    exitCode: number | null;
    fullOutput: string;
  },
): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/terminal-commands/${commandId}/finish`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function getWorkbenchState(
  projectId: string,
): Promise<{ state: Partial<WorkbenchState>; session: { activeFilePaths: string[]; terminalCwd?: string; updatedAt?: string } }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/workbench-state`);
}

export async function updateWorkbenchState(
  projectId: string,
  state: Partial<WorkbenchState>,
  activeFilePaths: string[] = [],
  terminalCwd?: string,
): Promise<{ ok: boolean; state: Partial<WorkbenchState> }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/workbench-state`, {
    method: "PUT",
    body: JSON.stringify({
      state,
      activeFilePaths,
      terminalCwd,
    }),
  });
}

export async function getProjectTree(
  projectId: string,
  path = "",
): Promise<{ path: string; nodes: WorkspaceTreeNode[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/tree`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  if (path) url.searchParams.set("path", path);
  return fetchJson(url.toString());
}

export async function getProjectFile(projectId: string, path: string): Promise<ProjectFileBuffer> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/files`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("path", path);
  return fetchJson(url.toString());
}

export async function saveProjectFile(
  projectId: string,
  path: string,
  content: string,
): Promise<{ saved: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files`, {
    method: "PUT",
    body: JSON.stringify({ path, content }),
  });
}

export async function createProjectEntry(
  projectId: string,
  path: string,
  kind: "file" | "directory",
  content?: string,
): Promise<{ ok: boolean; path: string; kind: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/create`, {
    method: "POST",
    body: JSON.stringify({ path, kind, content }),
  });
}

export async function renameProjectEntry(
  projectId: string,
  oldPath: string,
  newPath: string,
): Promise<{ ok: boolean; oldPath: string; newPath: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/rename`, {
    method: "POST",
    body: JSON.stringify({ old_path: oldPath, new_path: newPath }),
  });
}

export async function deleteProjectEntry(
  projectId: string,
  path: string,
): Promise<{ ok: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/delete`, {
    method: "POST",
    body: JSON.stringify({ path }),
  });
}

export async function searchProject(
  projectId: string,
  query: string,
  limit = 100,
): Promise<{ query: string; results: SearchResultItem[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/search`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("q", query);
  url.searchParams.set("limit", String(limit));
  return fetchJson(url.toString());
}

export async function getProjectProblems(
  projectId: string,
): Promise<{ items: ProblemItem[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/problems`);
}

export async function getOutputChannels(
  projectId: string,
): Promise<{ channels: OutputChannel[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/output/channels`);
}

export async function getOutputEvents(
  projectId: string,
  channel: string,
  limit = 100,
): Promise<{ channel: string; events: OutputEvent[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/output/events`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("channel", channel);
  url.searchParams.set("limit", String(limit));
  return fetchJson(url.toString());
}

export async function getProjectPorts(
  projectId: string,
): Promise<{ ports: PortEntry[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/ports`);
}

// ── Port Allocations (registro centralizzato, mig 0114) ──────────────────────

export interface PortAllocation {
  id: string;
  port: number;
  label: string;
  allocation_mode: "auto" | "manual";
  run_config_id: string | null;
  service_unit: string | null;
}

/** Lista porte allocate (registro persistente) per un progetto. */
export async function getPortAllocations(
  projectId: string,
): Promise<{ allocations: PortAllocation[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/port-allocations`);
}

/** Alloca una porta al progetto (manuale o auto). */
export async function createPortAllocation(
  projectId: string,
  port: number,
  label: string,
  mode: "auto" | "manual" = "manual",
): Promise<{ ok: boolean; allocation: PortAllocation }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/port-allocations`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ port, label, mode }),
    },
  );
}

/** Rilascia una porta allocata al progetto. */
export async function deletePortAllocation(
  projectId: string,
  port: number,
): Promise<{ ok: boolean }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/port-allocations/${port}`,
    { method: "DELETE" },
  );
}

/** Disinstalla un servizio systemd del progetto (stop+disable+rimuove unit file). */
export async function uninstallProjectService(
  projectId: string,
  service: string,
): Promise<{ ok: boolean; unit: string; path: string; removed: boolean }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/services/${encodeURIComponent(service)}`,
    { method: "DELETE" },
  );
}

/** Riavvia in batch tutti i servizi systemd `{slug}-*.service` del progetto. */
export async function restartAllProjectServices(
  projectId: string,
): Promise<{ slug: string; restarted: Array<{ unit: string; ok: boolean; stderr: string }> }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services/restart-all`, { method: "POST" });
}

/** Lista i file del progetto modificati dopo `since` (Unix ms). Esclude dirs di build. */
export async function getProjectChanges(
  projectId: string,
  since: number,
): Promise<{ since: number; count: number; changed: Array<{ path: string; mtime: number }> }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/changes?since=${since}`);
}

/** Termina i processi che occupano porte conflittuali (non gestiti dai servizi del progetto). */
export async function cleanupProjectPorts(
  projectId: string,
  ports?: number[],
): Promise<{
  slug: string;
  killed: Array<{ port: number; pid: number; program: string }>;
  skipped: Array<{ port: number; pid: number; program: string; reason: string }>;
}> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services/cleanup-ports`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: ports && ports.length ? JSON.stringify({ ports }) : "{}",
  });
}

export async function getPlaywrightRuns(
  projectId: string,
): Promise<{ runs: PlaywrightRunSummary[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/playwright/runs`);
}

export type DetectRunConfigsSource = "heuristic" | "ai" | "cached";

export async function detectRunConfigs(
  projectId: string,
  opts: { useAi?: boolean; force?: boolean } = {},
): Promise<{ suggestions: Omit<RunConfigItem, "id">[]; source?: DetectRunConfigsSource }> {
  const parts: string[] = [];
  if (opts.useAi)  parts.push("use_ai=1");
  if (opts.force)  parts.push("force=1");
  const qs = parts.length > 0 ? `?${parts.join("&")}` : "";
  return fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs/detect${qs}`);
}

export async function getRunConfigs(
  projectId: string,
): Promise<{ configs: RunConfigItem[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs`);
}

export async function createRunConfig(
  projectId: string,
  body: Omit<RunConfigItem, "id">,
): Promise<RunConfigItem> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export async function updateRunConfig(
  projectId: string,
  configId: string,
  body: Omit<RunConfigItem, "id">,
): Promise<void> {
  await fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs/${configId}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export async function deleteRunConfig(
  projectId: string,
  configId: string,
): Promise<void> {
  await fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs/${configId}`, {
    method: "DELETE",
  });
}

export async function launchRunConfig(
  projectId: string,
  configId: string,
): Promise<{ processId: string; channelId: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/run-configs/${configId}/launch`, {
    method: "POST",
  });
}

export async function getGitStatus(
  projectId: string,
): Promise<{ projectId: string; canManageGit: boolean; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/status`);
}

export async function getGitBranches(projectId: string): Promise<{ branches: GitBranchInfo[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/branches`);
}

export async function getGitLog(projectId: string): Promise<{ entries: GitLogEntry[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/log`);
}

export async function getGitDiff(
  projectId: string,
  path: string,
  staged = false,
): Promise<GitDiffResponse> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/git/diff`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("path", path);
  if (staged) {
    url.searchParams.set("staged", "true");
  }
  return fetchJson(url.toString());
}

export async function getGitUiPreferences(projectId: string): Promise<GitUiPreferences> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/preferences/git-ui`);
}

export async function updateGitUiPreferences(
  projectId: string,
  showHunkMap: boolean,
): Promise<{ ok: boolean; showHunkMap: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/preferences/git-ui`, {
    method: "PUT",
    body: JSON.stringify({ show_hunk_map: showHunkMap }),
  });
}

export async function stageGitPaths(projectId: string, paths: string[]) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/stage`, {
    method: "POST",
    body: JSON.stringify({ paths }),
  });
}

export async function unstageGitPaths(projectId: string, paths: string[]) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/unstage`, {
    method: "POST",
    body: JSON.stringify({ paths }),
  });
}

export async function commitGit(projectId: string, message: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/commit`, {
    method: "POST",
    body: JSON.stringify({ message }),
  });
}

export async function createGitBranch(projectId: string, name: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/branch`, {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export async function checkoutGitBranch(projectId: string, name: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/checkout`, {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export async function pullGit(projectId: string, remote?: string, branch?: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/pull`, {
    method: "POST",
    body: JSON.stringify({ remote, branch }),
  });
}

export async function pushGit(projectId: string, remote?: string, branch?: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/push`, {
    method: "POST",
    body: JSON.stringify({ remote, branch }),
  });
}

export async function getGitHubAccount(): Promise<{ account: GitHubAccountStatus }> {
  return fetchJson(`${API_BASE}/api/github/account`);
}

export async function listUserGitHubRepositories(): Promise<{ repositories: GitHubRepositorySummary[] }> {
  return fetchJson(`${API_BASE}/api/github/repositories`);
}

export async function connectGitHub(
  returnTo?: string,
): Promise<{ url: string }> {
  return fetchJson(`${API_BASE}/api/github/connect`, {
    method: "POST",
    body: JSON.stringify({ returnTo }),
  });
}

export async function disconnectGitHub(): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/github/account`, {
    method: "DELETE",
  });
}

export async function getProjectGitHubStatus(
  projectId: string,
): Promise<{ github: GitHubRemoteStatus }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/status`);
}

export async function listProjectGitHubRepositories(
  projectId: string,
): Promise<{ repositories: GitHubRepositorySummary[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/repositories`);
}

export async function cloneProjectGitHubRepository(
  projectId: string,
  payload: { cloneUrl: string },
): Promise<{ ok: boolean; repository: { owner: string; repo: string; cloneUrl: string }; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/clone`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function publishGitHubBranch(
  projectId: string,
): Promise<{ ok: boolean; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/publish-branch`, {
    method: "POST",
  });
}

export async function createGitHubPullRequest(
  projectId: string,
  payload: { title: string; body?: string; baseBranch?: string },
): Promise<{ created: boolean; pullRequest: GitHubPullRequestSummary }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/pull-request`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

// --- MCP Connectors ---

export interface McpServerTool {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
}

export interface McpServer {
  id: string;
  pluginInstanceId?: string;
  name: string;
  description?: string;
  iconUrl?: string;
  transport: "http" | "stdio";
  url?: string;
  command?: string;
  args: string[];
  envVars: Record<string, string>;
  headers: Record<string, string>;
  enabled: boolean;
  scope: "user" | "project" | "global";
  canManage?: boolean;
  createdAt: string;
  tools?: McpServerTool[];
}

export interface CreateMcpServerPayload {
  name: string;
  description?: string;
  transport: "http" | "stdio";
  url?: string;
  command?: string;
  args?: string[];
  envVars?: Record<string, string>;
  headers?: Record<string, string>;
  scope?: "user" | "project";
  projectId?: string;
}

export async function listMcpServers(): Promise<{ servers: McpServer[] }> {
  return fetchJson(`${API_BASE}/api/mcp-servers`);
}

export async function createMcpServer(payload: CreateMcpServerPayload): Promise<McpServer> {
  return fetchJson(`${API_BASE}/api/mcp-servers`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function updateMcpServer(
  serverId: string,
  updates: Partial<CreateMcpServerPayload & { enabled: boolean }>,
): Promise<McpServer> {
  return fetchJson(`${API_BASE}/api/mcp-servers/${serverId}`, {
    method: "PUT",
    body: JSON.stringify(updates),
  });
}

export async function deleteMcpServer(serverId: string): Promise<{ deleted: boolean }> {
  return fetchJson(`${API_BASE}/api/mcp-servers/${serverId}`, { method: "DELETE" });
}

export async function toggleMcpServer(
  serverId: string,
  enabled: boolean,
): Promise<{ id: string; enabled: boolean }> {
  return fetchJson(`${API_BASE}/api/mcp-servers/${serverId}/toggle`, {
    method: "PUT",
    body: JSON.stringify({ enabled }),
  });
}

export async function testMcpServer(
  serverId: string,
): Promise<{ success: boolean; toolCount: number; tools: McpServerTool[]; error?: string }> {
  return fetchJson(`${API_BASE}/api/mcp-servers/${serverId}/test`, { method: "POST" });
}

// --- Plugin Manager ---

export interface PluginRelease {
  id?: string;
  version: string;
  changelog?: string;
  isStable?: boolean;
  createdAt?: string;
}

export interface PluginCatalogItem {
  id: string;
  slug: string;
  name: string;
  description: string;
  pluginType: "mcp";
  transport: "http" | "stdio";
  httpUrl?: string;
  stdioCommand?: string;
  stdioArgs?: string[];
  requiredSecretRefs: string[];
  optionalSecretRefs: string[];
  defaultScope: "global" | "project" | "user";
  allowedCommands: string[];
  defaultToolPolicy: {
    mode: "allowlist" | "denylist" | "all";
    tools: string[];
    blockedTools: string[];
  };
  metadata?: Record<string, unknown>;
  isAllowlisted: boolean;
  enabled: boolean;
  releases: PluginRelease[];
}

export interface PluginToolPolicy {
  mode: "allowlist" | "denylist" | "all";
  tools: string[];
  blockedTools: string[];
}

export interface PluginInstance {
  id: string;
  catalogItemId?: string;
  releaseId?: string;
  version?: string;
  slug: string;
  catalogName: string;
  catalogDescription: string;
  transport: "http" | "stdio";
  scope: "global" | "project" | "user";
  projectId?: string;
  name: string;
  enabled: boolean;
  healthStatus: "unknown" | "ok" | "error";
  lastHealthMessage?: string;
  lastTestedAt?: string;
  mcpServerId?: string;
  toolPolicy: PluginToolPolicy;
  secretBindingsMasked: boolean;
  createdAt?: string;
  updatedAt?: string;
  canManage?: boolean;
}

export interface PluginHealthRun {
  id?: string;
  success: boolean;
  toolCount: number;
  errorMessage?: string;
  details?: Record<string, unknown>;
  createdAt?: string;
}

export interface PluginHealth {
  pluginInstanceId: string;
  status: "unknown" | "ok" | "error";
  lastMessage?: string;
  lastTestedAt?: string;
  runs: PluginHealthRun[];
}

export interface FigmaOAuthStatus {
  configured: boolean;
  hasClientId: boolean;
  hasClientSecret: boolean;
  hasAccessToken: boolean;
  tokenType: "pat" | "oauth_or_unknown";
  tokenScope?: string;
  tokenExpiresAt?: string;
  lastError?: string;
  redirectUri: string;
  preferStdioFallback: boolean;
}

export interface InstallPluginPayload {
  catalogItemId?: string;
  slug?: string;
  version?: string;
  scope?: "global" | "project" | "user";
  projectId?: string;
  name?: string;
  config?: Record<string, unknown>;
  secretBindings?: {
    headers?: Record<string, string>;
    envVars?: Record<string, string>;
  };
}

export async function listPluginCatalog(): Promise<{ items: PluginCatalogItem[] }> {
  return fetchJson(`${API_BASE}/api/plugins/catalog`);
}

export async function listInstalledPlugins(): Promise<{ items: PluginInstance[] }> {
  return fetchJson(`${API_BASE}/api/plugins/installed`);
}

export async function getFigmaOAuthStatus(): Promise<FigmaOAuthStatus> {
  return fetchJson(`${API_BASE}/api/plugins/figma/oauth/status`);
}

export async function startFigmaOAuth(
  returnTo?: string,
): Promise<{ url: string; redirectUri: string }> {
  return fetchJson(`${API_BASE}/api/plugins/figma/oauth/connect`, {
    method: "POST",
    body: JSON.stringify({ returnTo }),
  });
}

export async function installPlugin(payload: InstallPluginPayload): Promise<{
  ok: boolean;
  pluginInstanceId: string;
  mcpServerId: string;
  name: string;
  slug: string;
  version: string;
}> {
  return fetchJson(`${API_BASE}/api/plugins/install`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function updatePluginVersion(
  pluginInstanceId: string,
  version: string,
): Promise<{ ok: boolean; version: string }> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}/update`, {
    method: "POST",
    body: JSON.stringify({ version }),
  });
}

export async function uninstallPlugin(
  pluginInstanceId: string,
): Promise<{ ok: boolean; pluginInstanceId: string }> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}`, {
    method: "DELETE",
  });
}

export async function togglePlugin(
  pluginInstanceId: string,
  enabled: boolean,
): Promise<{ ok: boolean; enabled: boolean }> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}/toggle`, {
    method: "PUT",
    body: JSON.stringify({ enabled }),
  });
}

export async function testPlugin(
  pluginInstanceId: string,
): Promise<{ success: boolean; toolCount: number; tools: McpServerTool[]; error?: string }> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}/test`, {
    method: "POST",
  });
}

export async function getPluginHealth(pluginInstanceId: string): Promise<PluginHealth> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}/health`);
}

export async function updatePluginToolPolicy(
  pluginInstanceId: string,
  payload: PluginToolPolicy,
): Promise<{ ok: boolean; mode: string; tools: string[]; blockedTools: string[] }> {
  return fetchJson(`${API_BASE}/api/plugins/${pluginInstanceId}/tool-policy`, {
    method: "PUT",
    body: JSON.stringify({
      mode: payload.mode,
      tools: payload.tools,
      blockedTools: payload.blockedTools,
    }),
  });
}

export async function migrateLegacyMcpServerToPlugin(
  mcpServerId: string,
): Promise<{ ok: boolean; alreadyMigrated?: boolean; linkedExisting?: boolean; pluginInstanceId: string; slug?: string }> {
  return fetchJson(`${API_BASE}/api/plugins/migrate-legacy/${mcpServerId}`, {
    method: "POST",
  });
}

// --- Plugin integration (admin) ---

export interface IntegratePluginDraftPayload {
  slug: string;
  name: string;
  description?: string;
  transport: "http" | "stdio";
  httpUrl?: string;
  headers?: Record<string, string>;
  stdioCommand?: string;
  stdioArgs?: string[];
  envVars?: Record<string, string>;
  defaultScope?: "global" | "project" | "user";
  requiredSecretRefs?: string[];
  optionalSecretRefs?: string[];
  metadata?: Record<string, unknown>;
}

export interface IntegratePluginDraftResult {
  item: Record<string, unknown>;
  discoveredTools: Array<{
    name: string;
    description?: string;
    inputSchema?: unknown;
  }>;
  toolCount: number;
}

export async function draftPluginIntegration(
  payload: IntegratePluginDraftPayload,
): Promise<IntegratePluginDraftResult> {
  return fetchJson(`${adminServiceUrl("/plugins/integrate/draft")}`, {
    method: "POST",
    body: JSON.stringify({
      slug: payload.slug,
      name: payload.name,
      description: payload.description ?? "",
      transport: payload.transport,
      httpUrl: payload.httpUrl,
      headers: payload.headers ?? {},
      stdioCommand: payload.stdioCommand,
      stdioArgs: payload.stdioArgs ?? [],
      envVars: payload.envVars ?? {},
      defaultScope: payload.defaultScope ?? "global",
      requiredSecretRefs: payload.requiredSecretRefs ?? [],
      optionalSecretRefs: payload.optionalSecretRefs ?? [],
      metadata: payload.metadata ?? {},
    }),
  });
}

export async function publishPluginIntegration(payload: {
  item: Record<string, unknown>;
  version?: string;
  changelog?: string;
}): Promise<{ ok: boolean; catalogItemId: string; slug: string; version: string }> {
  return fetchJson(`${adminServiceUrl("/plugins/integrate/publish")}`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

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

export interface BillingUsageSummary {
  total_tokens: number;
  total_cost: number;
  total_runs: number;
}

export interface BillingUsageBreakdownItem {
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

// --- Neural Core (Python :8001) ---

export interface IntentResponse {
  intent: string;
  confidence: string;
}

export interface RouteResponse {
  intent: string;
  provider: string;
  model: string;
  rationale: string;
  confidence: string;
}

export interface ProviderModelsResponse {
  provider: string;
  status: string;
  models: string[];
}

export async function classifyIntent(
  projectId: string,
  profileId: string,
  message: string,
): Promise<IntentResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/classify-intent`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
  });
}

export async function routeModel(
  projectId: string,
  profileId: string,
  message: string,
): Promise<RouteResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/route-model`, {
    method: "POST",
    body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
  });
}

export async function getProviderModels(provider: string): Promise<ProviderModelsResponse> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers/${provider}/models`);
}

export async function getProviderHealth(provider: string): Promise<Record<string, unknown>> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/providers/${provider}/health`);
}

export async function getNeuralHealth(): Promise<Record<string, string>> {
  return fetchJsonNoAuth(`${NEURAL_BASE}/health`);
}

export async function getAgentRun(runId: string): Promise<AgentRunInfo> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}`);
}

export async function getActiveRunForSession(
  sessionId: string,
): Promise<{ activeRun: AgentRunInfo | null }> {
  return fetchJson(`${API_BASE}/api/chat/sessions/${sessionId}/active-run`);
}

export async function confirmAgentRun(
  runId: string,
  approved: boolean,
): Promise<{ runId: string; status: string }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/confirm`, {
    method: "POST",
    body: JSON.stringify({ approved }),
  });
}

export async function cancelAgentRun(
  runId: string,
): Promise<{ runId: string; status: string }> {
  return fetchJson(`${API_BASE}/api/chat/agent-runs/${runId}/cancel`, {
    method: "POST",
  });
}

export function subscribeAgentStream(
  sessionId: string,
  runId: string,
  onStep: (event: { runId: string; step: AgentStep | null; isFinal: boolean }) => void,
  onDone?: () => void,
  onTrace?: (trace: AITraceEvent) => void,
  onReconnecting?: (isReconnecting: boolean) => void,
  onToken?: (delta: string) => void,
): () => void {
  const url = `${API_BASE}/api/chat/sessions/${sessionId}/agent-stream?run_id=${runId}`;
  const es = new EventSource(url, { withCredentials: true });

  es.addEventListener("agent_step", (e) => {
    try {
      const data = JSON.parse((e as MessageEvent).data);
      onStep(data);
    } catch {}
  });

  es.addEventListener("agent_trace", (e) => {
    try {
      const data = JSON.parse((e as MessageEvent).data);
      const trace = (data.trace ?? data) as AITraceEvent;
      onTrace?.(trace);
    } catch {}
  });

  es.addEventListener("agent_token", (e) => {
    try {
      const data = JSON.parse((e as MessageEvent).data);
      onToken?.(data.delta as string);
    } catch {}
  });

  let receivedFinal = false;

  es.addEventListener("agent_final", (e) => {
    receivedFinal = true;
    try {
      const data = JSON.parse((e as MessageEvent).data);
      onStep(data);
    } catch {}
    es.close();
    onDone?.();
  });

  es.onerror = () => {
    es.close();
    if (receivedFinal) {
      onDone?.();
      return;
    }
    // SSE dropped before agent_final — poll DB to get final status
    // (handles server restart mid-run)
    onReconnecting?.(true);
    let attempts = 0;
    const maxAttempts = 20;
    const poll = () => {
      attempts++;
      getAgentRun(runId)
        .then((run) => {
          const isTerminal =
            run.status === "completed" ||
            run.status === "failed" ||
            run.status === "timed_out" ||
            run.status === "cancelled" ||
            run.status === "interrupted";
          if (isTerminal) {
            onReconnecting?.(false);
            onStep({ runId, step: null, isFinal: true });
            onDone?.();
          } else if (attempts < maxAttempts) {
            // Still running — retry in 2s (server might be rebooting)
            setTimeout(poll, 2000);
          } else {
            onReconnecting?.(false);
            onDone?.();
          }
        })
        .catch(() => {
          if (attempts < maxAttempts) {
            setTimeout(poll, 3000);
          } else {
            onReconnecting?.(false);
            onDone?.();
          }
        });
    };
    // First poll after 1s to give server time to restart
    setTimeout(poll, 1000);
  };

  return () => es.close();
}

// ── Profili utente (GPT/Gem style) ────────────────────────────────────────

export interface UserProfile {
  id: string;
  userId: string;
  name: string;
  description?: string;
  avatarEmoji: string;
  systemPrompt: string;
  defaultProvider?: string;
  defaultModel?: string;
  defaultAutomation?: "study" | "confirm" | "automatic";
  isDefault: boolean;
  isSystem: boolean;
  sourceTemplateKey?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface CreateProfilePayload {
  name: string;
  description?: string;
  avatarEmoji?: string;
  systemPrompt?: string;
  defaultProvider?: string;
  defaultModel?: string;
  defaultAutomation?: string;
}

export type UpdateProfilePayload = Partial<CreateProfilePayload>;

export async function getProfiles(): Promise<{ profiles: UserProfile[] }> {
  return fetchJson(`${API_BASE}/api/profiles`);
}

export async function createProfile(payload: CreateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/profiles`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function updateProfile(id: string, payload: UpdateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/profiles/${id}`, {
    method: "PUT",
    body: JSON.stringify(payload),
  });
}

export async function deleteProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/profiles/${id}`, { method: "DELETE" });
}

export async function setDefaultProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/profiles/${id}/default`, { method: "POST" });
}

export async function forkProfile(id: string): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/profiles/${id}/fork`, { method: "POST" });
}

// ── Admin profile management ────────────────────────────────────────────────

export async function adminListProfiles(): Promise<{ profiles: UserProfile[] }> {
  return fetchJson(`${API_BASE}/api/admin/profiles`);
}

export async function adminCreateProfile(payload: CreateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/admin/profiles`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function adminUpdateProfile(id: string, payload: UpdateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${id}`, {
    method: "PUT",
    body: JSON.stringify(payload),
  });
}

export async function adminDeleteProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${id}`, { method: "DELETE" });
}

export interface GlobalMcpServer {
  id: string;
  name: string;
  description?: string;
  transport: string;
  scope: string;
  enabled: boolean;
}

export async function adminListUserProfiles(): Promise<{ profiles: (UserProfile & { userEmail?: string })[] }> {
  return fetchJson(`${API_BASE}/api/admin/user-profiles`);
}

export async function adminListGlobalMcpServers(): Promise<{ servers: GlobalMcpServer[] }> {
  return fetchJson(`${API_BASE}/api/admin/global-mcp-servers`);
}

export async function adminGetProfileMcpServers(profileId: string): Promise<{ servers: GlobalMcpServer[] }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${profileId}/mcp-servers`);
}

export async function adminSetProfileMcpServers(
  profileId: string,
  mcpServerIds: string[],
): Promise<{ ok: boolean; count: number }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${profileId}/mcp-servers`, {
    method: "PUT",
    body: JSON.stringify({ mcpServerIds }),
  });
}

export async function setProjectDefaultProfile(
  projectId: string,
  profileId: string | null,
): Promise<{ ok: boolean; profileId: string | null }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/default-profile`, {
    method: "PATCH",
    body: JSON.stringify({ profileId: profileId ?? "" }),
  });
}

export interface QualityFinding {
  id: string;
  filePath: string;
  category: string;
  severity: "high" | "medium" | "low";
  title: string;
  detail: string;
  lineNumber: number | null;
  fixedAt: string | null;
  rule_key?: string;
}

export interface QualityScanResult {
  projectId: string;
  totalFindings: number;
  filesScanned: number;
  bySeverity: Record<string, number>;
  byCategory: Record<string, number>;
}

export async function runQualityScan(projectId: string): Promise<QualityScanResult> {
  const start: { scan_id?: number; status?: string } = await fetchJson(
    `${API_BASE}/api/projects/${projectId}/quality-scan`,
    { method: "POST" },
  );
  if (start.scan_id != null) {
    return pollQualityScanStatus(projectId, start.scan_id);
  }
  return start as unknown as QualityScanResult;
}

async function pollQualityScanStatus(projectId: string, scanId: number): Promise<QualityScanResult> {
  const maxAttempts = 120;
  for (let i = 0; i < maxAttempts; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    const poll: {
      status: string;
      totalFindings?: number;
      filesScanned?: number;
      bySeverity?: Record<string, number>;
      byCategory?: Record<string, number>;
      errorMessage?: string;
    } = await fetchJson(`${API_BASE}/api/projects/${projectId}/quality-scan/${scanId}`);
    if (poll.status === "completed") {
      return {
        projectId,
        totalFindings: poll.totalFindings ?? 0,
        filesScanned: poll.filesScanned ?? 0,
        bySeverity: poll.bySeverity ?? {},
        byCategory: poll.byCategory ?? {},
      };
    }
    if (poll.status === "failed") {
      throw new Error(poll.errorMessage ?? "Scansione fallita");
    }
  }
  throw new Error("Timeout scansione qualita'");
}

export async function getQualityFindings(
  projectId: string,
  opts: { severity?: string; category?: string; limit?: number } = {}
): Promise<{ findings: QualityFinding[]; total: number }> {
  const params = new URLSearchParams();
  if (opts.severity) params.set("severity", opts.severity);
  if (opts.category) params.set("category", opts.category);
  if (opts.limit) params.set("limit", String(opts.limit));
  const qs = params.toString() ? `?${params}` : "";
  return fetchJson(`${API_BASE}/api/projects/${projectId}/quality-findings${qs}`);
}

export async function markFindingFixed(projectId: string, findingId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/projects/${projectId}/quality-findings/${findingId}/mark-fixed`, { method: "POST" });
}

/**
 * Analizza un singolo file del progetto e restituisce i finding senza toccare il DB.
 * Usato per la verifica post-fix: controlla se un problema è stato effettivamente risolto
 * prima di marcarlo come fixed.
 */
export async function scanProjectFile(
  projectId: string,
  filePath: string
): Promise<{ findings: Omit<QualityFinding, "id" | "fixedAt">[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/quality-scan-file`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ file_path: filePath }),
  });
}

/**
 * Legge un intervallo di righe di un file del progetto.
 * Usato per includere il codice contestuale nei messaggi di fix.
 */
export async function getIndexStatus(
  projectId: string
): Promise<{ stale: string[]; staleCount: number; upToDate: number; notIndexed: number; totalFiles: number }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/index-status`);
}

export async function triggerReindexStale(
  projectId: string
): Promise<{ reindexed: number; skipped: number; total: number }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/reindex-stale`, { method: "POST" });
}

export async function readProjectFileLines(
  projectId: string,
  filePath: string,
  startLine: number,
  endLine: number
): Promise<{ lines: string; startLine: number; endLine: number }> {
  const params = new URLSearchParams({
    path: filePath,
    start: String(startLine),
    end: String(endLine),
  });
  return fetchJson(`${API_BASE}/api/projects/${projectId}/file-lines?${params}`);
}

export async function submitDeepReview(projectId: string): Promise<{ jobName: string; jobId: string; fileCount: number; status: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/deep-review`, { method: "POST" });
}

export async function getDeepReviewStatus(projectId: string, jobId: string): Promise<{
  state: string;
  completed: number;
  total: number;
  results?: Array<{
    path: string;
    issues: Array<{ line: number; severity: string; category: string; message: string; suggestion: string }>;
  }>;
}> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/deep-review/${encodeURIComponent(jobId)}`);
}


// ── Model Catalog ──────────────────────────────────────────────────────────

export interface ModelCatalogEntry {
  provider: string;
  model: string;
  displayName: string;
  inputCostPerMillion: number;
  outputCostPerMillion: number;
  currency: string;
  performanceTier: "light" | "medium" | "heavy";
  speedTier: "fast" | "medium" | "slow";
  capabilities: string[];
  contextWindow: number;
  supportsToolUse: boolean;
  batchDiscountPct: number;
  isFeatured: boolean;
  isEnabled: boolean;
}

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
}

export async function getModels(provider?: string): Promise<{ models: ModelCatalogEntry[] }> {
  const url = provider
    ? `${API_BASE}/api/models?provider=${encodeURIComponent(provider)}`
    : `${API_BASE}/api/models`;
  return fetchJson(url);
}

export async function getRoutingPreview(mode: string): Promise<RoutingPreviewResponse> {
  return fetchJson(`${API_BASE}/api/models/routing-preview?mode=${encodeURIComponent(mode)}`);
}

export async function syncModelCatalog(): Promise<{ updated: number; added: number; skipped: number }> {
  return fetchJson(`${API_BASE}/api/admin/sync-model-catalog`, { method: "POST" });
}

// ── Admin Users Management ─────────────────────────────────────────────────

export interface AdminUser {
  id: string;
  email: string;
  displayName: string;
  githubUsername?: string;
  avatarUrl?: string;
  role: "viewer" | "editor" | "admin";
  createdAt: string;
  lastActivityAt?: string;
}

export interface UserProject {
  projectId: string;
  projectName: string;
  role: string;
}

export interface AdminUserWithProjects extends AdminUser {
  projectCount: number;
  projects: UserProject[];
}

export interface ProjectMember {
  userId: string;
  email: string;
  displayName: string;
  githubUsername?: string;
  avatarUrl?: string;
  role: string;
  createdAt: string;
}

export async function listAdminUsers(page: number = 1, limit: number = 20): Promise<{ users: AdminUser[]; total: number; page: number; limit: number }> {
  return fetchJson(`${API_BASE}/api/admin/users?page=${page}&limit=${limit}`);
}

export async function getAdminUser(userId: string): Promise<AdminUserWithProjects> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}`);
}

export async function updateAdminUser(userId: string, data: { email?: string; displayName?: string }): Promise<AdminUser> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}`, {
    method: "PUT",
    body: JSON.stringify(data),
  });
}

export async function updateAdminUserRole(userId: string, role: string): Promise<AdminUser> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}/role`, {
    method: "PUT",
    body: JSON.stringify({ role }),
  });
}

export async function deleteAdminUser(userId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/admin/users/${userId}`, { method: "DELETE" });
}

export async function searchAdminUsers(query: string): Promise<{ users: AdminUser[] }> {
  return fetchJson(`${API_BASE}/api/admin/users/search?q=${encodeURIComponent(query)}`);
}

// ── Admin Projects Listing ─────────────────────────────────────────────────

export interface AdminProjectSummary {
  id: string;
  name: string;
  slug: string;
  ownerUserId: string;
  ownerEmail?: string | null;
  memberCount: number;
}

export async function listAdminProjects(): Promise<{ projects: AdminProjectSummary[] }> {
  return fetchJson(`${API_BASE}/api/admin/projects`);
}

// ── Admin Project Porting ──────────────────────────────────────────────────

export interface PortDetail {
  table: string;
  id: string;
  oldPath: string;
  newPath: string;
}

export interface PortProjectsResult {
  dryRun: boolean;
  projectsBaseRootUpdated: boolean;
  workspacesUpdated: number;
  repositoriesUpdated: number;
  details: PortDetail[];
  error?: string;
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

// ── Admin Project Members ──────────────────────────────────────────────────

export async function getProjectMembers(projectId: string): Promise<{ projectId: string; members: ProjectMember[] }> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members`);
}

export async function addProjectMember(projectId: string, userId: string, role: string): Promise<ProjectMember> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members`, {
    method: "POST",
    body: JSON.stringify({ user_id: userId, role }),
  });
}

export async function updateProjectMember(projectId: string, userId: string, role: string): Promise<ProjectMember> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members/${userId}`, {
    method: "PUT",
    body: JSON.stringify({ role }),
  });
}

export async function removeProjectMember(projectId: string, userId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members/${userId}`, { method: "DELETE" });
}

export async function markFindingFalsePositive(findingId: number | string, reason?: string, ruleKey?: string): Promise<void> {
  await fetch(`${API_BASE}/api/quality/findings/${findingId}/false-positive`, {
    method: 'POST',
    credentials: "include",
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ reason, rule_key: ruleKey }),
  });
}

export async function getFalsePositiveStats(): Promise<Array<{ rule_key: string; count: number }>> {
  const res = await fetch(`${API_BASE}/api/quality/false-positive-stats`, { credentials: "include" });
  if (!res.ok) return [];
  return res.json();
}

// ── Environment Status ─────────────────────────────────────────────────────

export interface EnvironmentCheck {
  id: string;
  label: string;
  status: "ok" | "warn" | "error" | "loading";
  detail: string;
}

export async function getEnvironmentStatus(): Promise<{ checks: EnvironmentCheck[] }> {
  return fetchJson(`${API_BASE}/api/admin/environment/status`);
}

export async function fixEnvironment(action: string, sudoPassword?: string): Promise<{ ok: boolean; output: string }> {
  // Le operazioni di fix (es. apt-get install) possono richiedere > 30s.
  // Usiamo un AbortController con timeout di 3 minuti invece del default 30s di fetchJson.
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 180_000); // 3 min
  try {
    return await fetchJson(`${API_BASE}/api/admin/environment/fix`, {
      method: "POST",
      body: JSON.stringify({ action, sudo_password: sudoPassword }),
      signal: controller.signal,
    });
  } finally {
    clearTimeout(timeoutId);
  }
}


export async function getGatewayProviders(): Promise<Record<string, unknown>> {
  return fetchJson(`${API_BASE}/api/gateway/providers`);
}

export async function reloadGatewayConfig(): Promise<Record<string, unknown>> {
  return fetchJson(`${API_BASE}/api/gateway/reload`, { method: "POST" });
}

// ── Project Database API ──────────────────────────────────────────────────────

export interface ProjectDbConfig {
  configured: boolean;
  project_id?: string;
  engine?: string | null;
  hosting_mode?: "internal" | "external" | null;
  migration_tool?: string | null;
  migration_path?: string | null;
  allow_ddl_override?: boolean;
  detection_metadata?: Record<string, unknown>;
  pending_count?: number;
  applied_count?: number;
}

export interface ProjectMigration {
  id: string;
  filename: string;
  checksum: string | null;
  status: "pending" | "pending_override" | "applied" | "rolled_back" | "overridden" | "failed";
  description?: string | null;
  created_by_agent?: string | null;
  created_at: string;
  applied_at?: string | null;
  error_message?: string | null;
}

export async function getProjectDbConfig(projectId: string): Promise<ProjectDbConfig> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db`);
}

export async function setProjectDbConfig(
  projectId: string,
  config: Partial<{
    engine: string;
    hosting_mode: string;
    migration_tool: string;
    migration_path: string;
    allow_ddl_override: boolean;
    connection_string: string;
    connection_host: string;
    connection_port: number;
    connection_database: string;
    connection_user: string;
    connection_password: string;
    name: string;
    is_primary: boolean;
  }>
): Promise<{ ok: boolean; name?: string; is_primary?: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/config`, {
    method: "POST",
    body: JSON.stringify(config),
  });
}

export interface ProjectDbConnection {
  id: string;
  name: string;
  engine?: string | null;
  hosting_mode?: string | null;
  migration_tool?: string | null;
  migration_path?: string | null;
  allow_ddl_override: boolean;
  is_primary: boolean;
}

export async function listProjectDbConnections(
  projectId: string
): Promise<{ connections: ProjectDbConnection[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/connections`);
}

export async function setPrimaryProjectDbConnection(
  projectId: string,
  connId: string
): Promise<{ ok: boolean }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/db/connections/${connId}/set-primary`,
    { method: "POST" }
  );
}

export async function deleteProjectDbConnection(
  projectId: string,
  connId: string
): Promise<{ ok: boolean }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/db/connections/${connId}`,
    { method: "DELETE" }
  );
}

export interface ProjectServiceEntry {
  unit: string;   // es. "redemptor-backend.service"
  short: string;  // es. "backend"
  state: string;  // "active" | "inactive" | "failed" | "activating" | ...
  sub: string;    // "running" | "exited" | "dead" | ...
  // Diagnostica crash-loop (popolata solo se il servizio e' in stato failing/failed)
  last_error?: string;
  suggestion?: string;
  error_kind?: string;
  crash_loop?: boolean; // true se il servizio appare active ma ha NRestarts > 2
}

export async function getProjectServicesStatus(
  projectId: string
): Promise<{ services: ProjectServiceEntry[]; slug: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services`);
}

export async function controlProjectService(
  projectId: string,
  service: string,   // nome corto, es. "api", "worker-email", "frontend-admin"
  action: "start" | "stop" | "restart"
): Promise<{ ok: boolean; unit: string; action: string; stdout: string; stderr: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services/${encodeURIComponent(service)}/${action}`, {
    method: "POST",
  });
}

export interface ServiceWizardSuggestion {
  short: string;       // nome corto, es. "backend"
  unit: string;        // nome unit completo, es. "redemptor-backend.service"
  label: string;       // descrizione leggibile
  kind: string;        // "npm" | "pnpm" | "dotnet" | "cargo" | "python" | "shell"
  command: string;
  args: string[];
  cwd: string;
  existing: boolean;   // true se il .service è già installato
}

export async function detectProjectServices(
  projectId: string
): Promise<{ suggestions: ServiceWizardSuggestion[]; slug: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services/wizard/detect`);
}

export async function installProjectService(
  projectId: string,
  svc: Omit<ServiceWizardSuggestion, "existing"> & { description?: string; env?: Record<string, string> }
): Promise<{ ok: boolean; unit: string; path: string; content: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/services/wizard/install`, {
    method: "POST",
    body: JSON.stringify(svc),
  });
}

export async function listProjectMigrations(projectId: string): Promise<{ migrations: ProjectMigration[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/migrations`);
}

export async function createProjectMigration(
  projectId: string,
  name: string,
  sql: string,
  description?: string
): Promise<{ ok: boolean; filename?: string; checksum?: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/migrations`, {
    method: "POST",
    body: JSON.stringify({ name, sql, description }),
  });
}

export async function applyProjectMigrations(
  projectId: string,
  filename?: string
): Promise<{ ok: boolean; applied?: string[] | { filename: string; status: string }[]; errors?: unknown[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/migrations/apply`, {
    method: "POST",
    body: JSON.stringify({ filename }),
  });
}

export async function rollbackProjectMigration(projectId: string): Promise<{ ok: boolean; rolled_back?: string; error?: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/migrations/rollback`, {
    method: "POST",
  });
}

export interface ProjectDbDetectResult {
  ok: boolean;
  engine?: string | null;
  migration_tool?: string | null;
  migration_path?: string | null;
  connection_string?: string | null;
  hosting_mode?: string | null;
  hints?: string[];
}

export async function detectProjectDb(projectId: string): Promise<ProjectDbDetectResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/detect`, {
    method: "POST",
  });
}

export interface ProjectDbTestResult {
  ok: boolean;
  engine?: string;
  server_version?: string | null;
  table_count?: number | null;
  latency_ms?: number;
  error?: string;
  hint?: string | null;
}

export async function testProjectDbConnection(
  projectId: string,
  body: { engine?: string; connection_string?: string; connection_id?: string; name?: string }
): Promise<ProjectDbTestResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/test-connection`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function requestProjectDbOverride(
  projectId: string,
  sql: string,
  reason: string
): Promise<{ ok: boolean; migration_id?: string; request_id?: string; filename?: string; warning?: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/override-request`, {
    method: "POST",
    body: JSON.stringify({ sql, reason }),
  });
}

// ── Servizi di sistema Nexus ─────────────────────────────────────────────────

export interface NexusServiceInfo {
  name: string;
  label: string;
  port: number;
  description: string;
  /** LED della statusbar controllato da questo servizio (es. "Tools", "Brain", "OpenAI · Anthropic · …"). */
  led?: string;
  /** Servizio system (postgres, redis): mostrabile ma non controllabile senza root. */
  readonly?: boolean;
  state: "active" | "inactive" | "failed" | "activating" | "unknown";
  sub_state?: string;
  /** true se la porta TCP risponde, indipendentemente dallo stato systemd. */
  port_alive?: boolean;
}

/** Recupera lo stato di tutti i servizi di sistema Nexus.
 *  Usa URL relativo (non API_BASE) perché l'endpoint è su Next.js,
 *  non su mcp-core. Funziona anche quando mcp-core è offline. */
export async function getNexusServicesStatus(): Promise<{ services: NexusServiceInfo[] }> {
  return fetchJsonNoAuth(`/api/system/services`, undefined, 20000);
}

/** Avvia, stoppa o riavvia un servizio Nexus.
 *  Usa URL relativo (non API_BASE) per lo stesso motivo. */
export async function controlNexusService(
  service: string,
  action: "start" | "stop" | "restart"
): Promise<{ ok: boolean; unit: string; action: string; stdout: string; stderr: string }> {
  return fetchJsonNoAuth(`/api/system/services/${encodeURIComponent(service)}/${action}`, {
    method: "POST",
  }, 30000);
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
