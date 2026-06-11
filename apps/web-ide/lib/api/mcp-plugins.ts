import { API_BASE, adminServiceUrl, fetchJson } from "./_shared";

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
  linkedTemplatesCount?: number;
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

export async function deleteMcpServer(serverId: string): Promise<{ deleted: boolean }> {
  return fetchJson(`${API_BASE}/api/mcp-servers/${serverId}`, { method: "DELETE" });
}

// --- Plugin Manager ---

interface PluginRelease {
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
