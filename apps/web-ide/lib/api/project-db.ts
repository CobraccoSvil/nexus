import { API_BASE, fetchJson } from "./_shared";

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

export interface ProvisionProjectDbResult {
  ok: boolean;
  mode?: string;
  name?: string;
  db_name?: string;
  dsn?: string;
  created?: boolean;
  is_primary?: boolean;
  engine?: string;
  server_version?: string | null;
  table_count?: number | null;
  error?: string;
}

/**
 * Provisiona davvero un database per il progetto.
 * - mode "internal": Nexus crea un Postgres isolato nel cluster dedicato
 *   (nessuna credenziale richiesta).
 * - mode "external": valida e registra la connection_string fornita.
 */
export async function provisionProjectDb(
  projectId: string,
  body: {
    mode: "internal" | "external";
    name?: string;
    db_name?: string;
    engine?: string;
    connection_string?: string;
  }
): Promise<ProvisionProjectDbResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/provision`, {
    method: "POST",
    body: JSON.stringify(body),
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

export async function listProjectMigrations(projectId: string): Promise<{ migrations: ProjectMigration[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/migrations`);
}

// ── SQL panel: esecuzione ad-hoc query sul DB applicativo del progetto ──────

export type SqlExecuteResult =
  | {
      ok: true;
      mode: "read";
      statement_kind: string;
      columns: Array<{ name: string; type: string }>;
      row_count: number;
      rows: Array<Record<string, unknown>>;
      truncated: boolean;
      duration_ms: number;
    }
  | {
      ok: true;
      mode: "write";
      statement_kind: string;
      rows_affected: number;
      duration_ms: number;
      hint?: string;
    };

/**
 * Esegue una query SQL ad-hoc sul DB applicativo del progetto.
 * Backend: `POST /api/projects/:id/db/query` (vedi
 * crates/mcp-core/src/project_db_routes.rs::execute_project_db_query).
 * La connessione e' risolta server-side da project_database_config
 * (guard-rail anti-Nexus presente). Limiti: timeout 30s, max 1000 righe.
 *
 * `connection` (opzionale): nome della connessione del progetto
 * (es. "primary", "analytics", "legacy_replica"). Se omesso o vuoto,
 * usa la connessione con is_primary=true.
 */
export async function executeProjectDbQuery(
  projectId: string,
  sql: string,
  params?: unknown[],
  maxRows?: number,
  connection?: string
): Promise<SqlExecuteResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/query`, {
    method: "POST",
    body: JSON.stringify({
      sql,
      params: params ?? [],
      max_rows: maxRows,
      connection: connection || undefined,
    }),
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

export interface ImportSchemaResult {
  ok: boolean;
  ambiguous?: boolean;
  candidates?: string[];
  message?: string;
  file?: string;
  statements_run?: number;
  tables_after?: number | null;
}

export async function importProjectDbSchema(
  projectId: string,
  filePath?: string,
  connection?: string
): Promise<ImportSchemaResult> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/db/import-schema`, {
    method: "POST",
    body: JSON.stringify({
      file_path: filePath || undefined,
      connection: connection || undefined,
    }),
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
