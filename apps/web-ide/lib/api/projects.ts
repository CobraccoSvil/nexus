import { API_BASE, fetchJson } from "./_shared";
import type { GitRepositoryState } from "./git";
import type { WorkspaceTreeNode } from "./workspace";

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

interface DeepAnalysisIssue {
  severity: "high" | "medium" | "low";
  title: string;
  files: string[];
  description: string;
  suggested_fix: string;
}

interface DeepAnalysisService {
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

export async function openProject(
  projectId: string,
): Promise<{ project: UserProjectDetails; tree: WorkspaceTreeNode[]; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/open`, { method: "POST" });
}

/** Lista i file del progetto modificati dopo `since` (Unix ms). Esclude dirs di build. */
export async function getProjectChanges(
  projectId: string,
  since: number,
): Promise<{ since: number; count: number; changed: Array<{ path: string; mtime: number }> }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/changes?since=${since}`);
}
