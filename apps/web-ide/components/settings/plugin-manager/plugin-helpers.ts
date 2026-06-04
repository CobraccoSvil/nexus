import type { McpServer, PluginCatalogItem, PluginInstance } from "../../../lib/api-client";
import type { CatalogEntry } from "../mcp-catalog-data";

export function normalizeCsv(raw: string): string[] {
  return raw
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

export function defaultReleaseVersion(item: PluginCatalogItem): string {
  const stable = item.releases.find((release) => release.isStable !== false);
  return stable?.version ?? item.releases[0]?.version ?? "1.0.0";
}

export function dedupeInstalledPlugins(items: PluginInstance[]): PluginInstance[] {
  const map = new Map<string, PluginInstance>();
  for (const item of items) {
    const key = (item.slug || item.catalogItemId || item.id).toLowerCase();
    const existing = map.get(key);
    if (!existing) {
      map.set(key, item);
      continue;
    }
    const existingTs = Date.parse(existing.updatedAt ?? existing.createdAt ?? "") || 0;
    const currentTs = Date.parse(item.updatedAt ?? item.createdAt ?? "") || 0;
    if (currentTs >= existingTs) {
      map.set(key, item);
    }
  }
  return Array.from(map.values());
}

export function healthColor(status: string) {
  if (status === "ok") return "#22c55e";
  if (status === "error") return "#ef4444";
  return "#94a3b8";
}

export function isNexusBrowserBridgeLocal(server: McpServer): boolean {
  if (server.transport !== "http") return false;
  const url = (server.url ?? "").trim().toLowerCase();
  return (
    url === "http://127.0.0.1:4055/mcp" ||
    url === "http://localhost:4055/mcp" ||
    url === "http://0.0.0.0:4055/mcp"
  );
}

export function detectLegacyMigratableSlug(server: McpServer): string | null {
  if (server.transport === "http") {
    const url = (server.url ?? "").toLowerCase();
    if (url.includes("mcp.figma.com/mcp")) return "figma-http";
    if (url.includes("api.githubcopilot.com/mcp")) return "github-http";
    return null;
  }

  const command = (server.command ?? "").toLowerCase();
  const args = (server.args ?? []).map((item) => item.toLowerCase());
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-filesystem"))) {
    return "filesystem-local";
  }
  if (command === "npx" && args.some((item) => item.includes("@playwright/mcp"))) {
    return "playwright-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-redis"))) {
    return "redis-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-sqlite"))) {
    return "sqlite-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-postgres"))) {
    return "postgres-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-gitlab"))) {
    return "gitlab-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-github"))) {
    return "github-stdio";
  }
  if (command === "npx" && args.some((item) => item.includes("@modelcontextprotocol/server-memory"))) {
    return "memory-stdio";
  }
  return null;
}

export function toLegacyPayload(entry: CatalogEntry) {
  const envVars =
    entry.requiredEnvVars?.reduce<Record<string, string>>((acc, key) => {
      acc[key] = "";
      return acc;
    }, {}) ?? {};

  return {
    name: entry.name,
    description: entry.description,
    transport: entry.transport,
    url: entry.transport === "http" ? entry.url : undefined,
    command: entry.transport === "stdio" ? entry.command : undefined,
    args: entry.transport === "stdio" ? entry.args ?? [] : [],
    envVars,
    headers: {},
    scope: "user" as const,
  };
}
