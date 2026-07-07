import { API_BASE, fetchJson, fetchJsonNoAuth } from "./_shared";

// --- MCP Core (Rust :4000) ---

export interface HealthResponse {
  status: string;
  components: {
    database: boolean;
    /** Redis cache/broker */
    redis: boolean;
    /** mcp-core (porta 4000): orchestratore + endpoint AI (/api/neural) + agent run.
     *  Dopo l'eliminazione del brain Python questo e' l'unico LED del Core. */
    neural_core: boolean;
    /** gRPC ToolRunner :50071 — se false, l'AI non puo' eseguire tool MCP */
    tools_grpc?: boolean;
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
  description?: string;
  /** LED della statusbar alimentato da questo servizio (es. "Core", "Redis", "DB"). */
  led?: string;
  /** Servizio infra readonly (postgres, redis): mostrato ma senza pulsanti di controllo. */
  readonly?: boolean;
  /** true se start/stop/restart sono ammessi (campo `controllable` del catalogo). */
  controllable?: boolean;
  state: "active" | "inactive" | "failed" | "activating" | "unknown";
  sub_state?: string;
  /** true se la porta TCP risponde. Coincide con state === "active": segnale
   *  onesto, non un override che maschera "unknown". */
  port_alive?: boolean;
}

/** Recupera lo stato di tutti i servizi di sistema Nexus.
 *  Il route Next.js proxya verso mcp-core, che calcola lo stato in modo
 *  platform-aware (TCP probe della porta dal catalogo DB). Se mcp-core e'
 *  offline il pannello mantiene l'ultimo stato noto. */
export async function getNexusServicesStatus(): Promise<{ services: NexusServiceInfo[] }> {
  return fetchJsonNoAuth(`/api/system/services`, undefined, 20000);
}

/** Avvia, stoppa o riavvia un servizio Nexus (proxy verso mcp-core, controllo
 *  platform-aware: systemctl su Unix, deploy/dev-service.ps1 su Windows). */
export async function controlNexusService(
  service: string,
  action: "start" | "stop" | "restart"
): Promise<{ ok: boolean; service?: string; action: string; stdout?: string; stderr?: string }> {
  return fetchJsonNoAuth(`/api/system/services/${encodeURIComponent(service)}/${action}`, {
    method: "POST",
  }, 30000);
}
