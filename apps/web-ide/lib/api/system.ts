import { API_BASE, fetchJson, fetchJsonNoAuth } from "./_shared";

// --- MCP Core (Rust :4000) ---

export interface HealthResponse {
  status: string;
  components: {
    database: boolean;
    redis: boolean;
    neural_core: boolean;
    /** gRPC ToolRunner :50071 — se false, l'AI non puo' eseguire tool MCP */
    tools_grpc?: boolean;
    /** Brain REST :8001 — se false, gli agent run non funzioneranno */
    brain_rest?: boolean;
  };
}

export async function getHealth(): Promise<HealthResponse> {
  return fetchJson(`${API_BASE}/api/health`);
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
