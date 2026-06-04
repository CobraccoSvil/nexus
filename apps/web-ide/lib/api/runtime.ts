import { API_BASE, fetchJson } from "./_shared";

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

export interface PlaywrightProgress {
  total?: number | null;
  passed: number;
  failed: number;
  skipped: number;
  flaky: number;
  current_spec?: string | null;
  failed_specs?: string[];
}

export interface PlaywrightRunSummary {
  id: string;
  label: string;
  status: string;
  summary?: string;
  createdAt: string;
  updatedAt?: string;
  artifacts?: PlaywrightArtifact[];
  progress?: PlaywrightProgress;
  command?: string;
  exitCode?: number;
  [k: string]: unknown;
}

export interface PlaywrightRunDetail extends PlaywrightRunSummary {
  outputLog: string;
}

export type PlaywrightArtifact = {
  path: string;
  kind: string;
  name?: string;
  [k: string]: unknown;
};

export interface TerminalSession {
  sessionId: string;
  token: string;
  workingDirectory: string;
  shell: string;
  expiresAt: number;
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

/** Termina il processo in ascolto sulla porta e rilascia l'allocazione. */
export async function killPortProcess(
  projectId: string,
  port: number,
): Promise<{ ok: boolean; port: number; freed: boolean; deleted_allocations: number }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/services/kill-port-process`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ port }),
    },
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
): Promise<{ runs: PlaywrightRunSummary[]; configured?: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/playwright/runs`);
}

export async function getPlaywrightRunDetail(
  projectId: string,
  runId: string,
): Promise<PlaywrightRunDetail> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/playwright/runs/${runId}`);
}

/**
 * Apre uno stream SSE per gli eventi live di un run Playwright.
 * Eventi emessi: "line" (riga output), "progress" (counter), "final" (esito).
 * Ritorna l'EventSource per consentire al chiamante di chiudere lo stream.
 */
export function subscribePlaywrightRunStream(
  projectId: string,
  runId: string,
  handlers: {
    onLine?: (data: { job_id: string; line: string }) => void;
    onProgress?: (data: { job_id: string; progress: PlaywrightProgress }) => void;
    onFinal?: (data: { job_id: string; status: string; exit_code: number; progress: PlaywrightProgress }) => void;
    onError?: (err: Event) => void;
  },
): EventSource {
  const url = `${API_BASE}/api/projects/${projectId}/playwright/runs/${runId}/stream`;
  const es = new EventSource(url, { withCredentials: true });
  if (handlers.onLine) {
    es.addEventListener("line", (e: MessageEvent) => {
      try { handlers.onLine!(JSON.parse(e.data)); } catch { /* ignore */ }
    });
  }
  if (handlers.onProgress) {
    es.addEventListener("progress", (e: MessageEvent) => {
      try { handlers.onProgress!(JSON.parse(e.data)); } catch { /* ignore */ }
    });
  }
  if (handlers.onFinal) {
    es.addEventListener("final", (e: MessageEvent) => {
      try { handlers.onFinal!(JSON.parse(e.data)); } catch { /* ignore */ }
    });
  }
  if (handlers.onError) {
    es.addEventListener("error", handlers.onError);
  }
  return es;
}

export async function clearPlaywrightRuns(_projectId?: string): Promise<{ ok: boolean }> {
  return { ok: true };
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
  env?: Record<string, string>; // env suggerito (es. PORT deterministico)
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
