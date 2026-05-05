"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  createMcpServer,
  deleteMcpServer,
  draftPluginIntegration,
  getFigmaOAuthStatus,
  installPlugin,
  listAdminSettings,
  listInstalledPlugins,
  listMcpServers,
  listPluginCatalog,
  migrateLegacyMcpServerToPlugin,
  publishPluginIntegration,
  testPlugin,
  togglePlugin,
  uninstallPlugin,
  updateAdminSetting,
  updatePluginToolPolicy,
  updatePluginVersion,
  startFigmaOAuth,
  type AdminSettingEntry,
  type FigmaOAuthStatus,
  type IntegratePluginDraftPayload,
  type IntegratePluginDraftResult,
  type McpServer,
  type PluginCatalogItem,
  type PluginInstance,
  type PluginToolPolicy,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { MCP_CATALOG, type CatalogEntry } from "./mcp-catalog-data";
import { useGlobalDialog } from "../global-dialog-provider";

type ManagerTab = "installed" | "catalog" | "policy" | "legacy";

interface PolicyDraft {
  mode: PluginToolPolicy["mode"];
  tools: string;
  blockedTools: string;
}

function normalizeCsv(raw: string): string[] {
  return raw
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function defaultReleaseVersion(item: PluginCatalogItem): string {
  const stable = item.releases.find((release) => release.isStable !== false);
  return stable?.version ?? item.releases[0]?.version ?? "1.0.0";
}

function dedupeInstalledPlugins(items: PluginInstance[]): PluginInstance[] {
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

function healthColor(status: string) {
  if (status === "ok") return "#22c55e";
  if (status === "error") return "#ef4444";
  return "#94a3b8";
}

function detectLegacyMigratableSlug(server: McpServer): string | null {
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

function toLegacyPayload(entry: CatalogEntry) {
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

export function PluginManager() {
  const tc = useThemeColors();
  const { confirmDialog, promptDialog } = useGlobalDialog();

  const [activeTab, setActiveTab] = useState<ManagerTab>("installed");
  const [catalog, setCatalog] = useState<PluginCatalogItem[]>([]);
  const [installed, setInstalled] = useState<PluginInstance[]>([]);
  const [legacyConnectors, setLegacyConnectors] = useState<McpServer[]>([]);
  const [adminSettings, setAdminSettings] = useState<AdminSettingEntry[]>([]);
  const [figmaOAuthStatus, setFigmaOAuthStatus] = useState<FigmaOAuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const [catalogSearch, setCatalogSearch] = useState("");
  const [installScope, setInstallScope] = useState<"global" | "project" | "user">("global");
  const [showAlreadyPresent, setShowAlreadyPresent] = useState(false);
  const [catalogReleaseChoice, setCatalogReleaseChoice] = useState<Record<string, string>>({});
  const [instanceReleaseChoice, setInstanceReleaseChoice] = useState<Record<string, string>>({});
  const [policyDrafts, setPolicyDrafts] = useState<Record<string, PolicyDraft>>({});
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});
  const [integrateDraft, setIntegrateDraft] = useState<IntegratePluginDraftResult | null>(null);

  const loadData = useCallback(async () => {
    setError(null);
    const [catalogRes, installedRes, legacyRes, settingsRes, figmaOauthRes] = await Promise.all([
      listPluginCatalog(),
      listInstalledPlugins(),
      listMcpServers(),
      listAdminSettings(),
      getFigmaOAuthStatus().catch(() => null),
    ]);

    const nextCatalogChoices: Record<string, string> = {};
    for (const item of catalogRes.items) {
      nextCatalogChoices[item.id] = defaultReleaseVersion(item);
    }
    setCatalogReleaseChoice((prev) => ({ ...nextCatalogChoices, ...prev }));

    const nextInstanceChoices: Record<string, string> = {};
    for (const plugin of installedRes.items) {
      nextInstanceChoices[plugin.id] = plugin.version ?? "";
    }
    setInstanceReleaseChoice((prev) => ({ ...nextInstanceChoices, ...prev }));

    setCatalog(catalogRes.items);
    setInstalled(dedupeInstalledPlugins(installedRes.items));
    setLegacyConnectors(legacyRes.servers.filter((server) => !server.pluginInstanceId));
    setAdminSettings(settingsRes.settings);
    setFigmaOAuthStatus(figmaOauthRes);
  }, []);

  useEffect(() => {
    let mounted = true;
    setLoading(true);
    void loadData()
      .catch((loadError) => {
        if (!mounted) return;
        setError(loadError instanceof Error ? loadError.message : "Errore caricamento plugin manager.");
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
    };
  }, [loadData]);

  useEffect(() => {
    setPolicyDrafts((prev) => {
      const next = { ...prev };
      for (const plugin of installed) {
        if (next[plugin.id]) continue;
        next[plugin.id] = {
          mode: plugin.toolPolicy?.mode ?? "all",
          tools: (plugin.toolPolicy?.tools ?? []).join(", "),
          blockedTools: (plugin.toolPolicy?.blockedTools ?? []).join(", "),
        };
      }
      return next;
    });
  }, [installed]);

  const catalogById = useMemo(
    () => new Map(catalog.map((item) => [item.id, item])),
    [catalog],
  );

  const installedSlugSet = useMemo(
    () => new Set(installed.map((item) => item.slug.toLowerCase())),
    [installed],
  );

  const filteredCatalog = useMemo(() => {
    const query = catalogSearch.trim().toLowerCase();
    if (!query) return catalog;
    return catalog.filter((item) => {
      return (
        item.name.toLowerCase().includes(query) ||
        item.slug.toLowerCase().includes(query) ||
        item.description.toLowerCase().includes(query)
      );
    });
  }, [catalog, catalogSearch]);

  const filteredLegacyCatalog = useMemo(() => {
    const query = catalogSearch.trim().toLowerCase();
    if (!query) return MCP_CATALOG;
    return MCP_CATALOG.filter((item) => {
      return (
        item.name.toLowerCase().includes(query) ||
        item.id.toLowerCase().includes(query) ||
        item.description.toLowerCase().includes(query) ||
        (item.tags ?? []).some((tag) => tag.toLowerCase().includes(query))
      );
    });
  }, [catalogSearch]);

  const legacyServerKeys = useMemo(() => {
    return new Set(
      legacyConnectors.map((server) => `${server.name.toLowerCase()}::${server.transport}`),
    );
  }, [legacyConnectors]);

  const visibleCuratedCatalog = useMemo(() => {
    if (showAlreadyPresent) return filteredCatalog;
    return filteredCatalog.filter((item) => !installedSlugSet.has(item.slug.toLowerCase()));
  }, [filteredCatalog, installedSlugSet, showAlreadyPresent]);

  const visibleLegacyPresetCatalog = useMemo(() => {
    if (showAlreadyPresent) return filteredLegacyCatalog;
    return filteredLegacyCatalog.filter((entry) => {
      const legacyKey = `${entry.name.toLowerCase()}::${entry.transport}`;
      return !legacyServerKeys.has(legacyKey);
    });
  }, [filteredLegacyCatalog, legacyServerKeys, showAlreadyPresent]);

  const requiredSecretRefs = useMemo(() => {
    const keys = new Set<string>();
    for (const item of catalog) {
      for (const key of item.requiredSecretRefs ?? []) {
        if (key.trim()) keys.add(key.trim());
      }
    }
    return Array.from(keys).sort();
  }, [catalog]);

  const settingsByKey = useMemo(() => {
    return new Map(adminSettings.map((setting) => [setting.key, setting]));
  }, [adminSettings]);
  const figmaPreferStdio = useMemo(() => {
    if (typeof figmaOAuthStatus?.preferStdioFallback === "boolean") {
      return figmaOAuthStatus.preferStdioFallback;
    }
    const raw = settingsByKey.get("figma_mcp_prefer_stdio")?.value ?? "true";
    const normalized = raw.trim().toLowerCase();
    return normalized === "1" || normalized === "true" || normalized === "yes" || normalized === "on";
  }, [figmaOAuthStatus?.preferStdioFallback, settingsByKey]);

  const handleInstall = useCallback(
    async (item: PluginCatalogItem) => {
      if (installedSlugSet.has(item.slug.toLowerCase())) {
        setInfo(`Plugin ${item.name} già installato. Usa "Installati" per aggiornarlo o gestirlo.`);
        return;
      }

      const version = catalogReleaseChoice[item.id] || defaultReleaseVersion(item);
      const busyId = `install:${item.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        let projectId: string | undefined;
        if (installScope === "project") {
          const value = await promptDialog(
            "Inserisci projectId per installazione scope project",
            "",
            `Installa ${item.name}`,
          );
          if (!value) return;
          projectId = value.trim();
        }

        const payload: Parameters<typeof installPlugin>[0] = {
          catalogItemId: item.id,
          version,
          scope: installScope,
          projectId,
        };

        if (item.slug === "figma-http") {
          payload.secretBindings = {
            headers: {
              Authorization: "figma_oauth_token",
              "X-Figma-Token": "figma_oauth_token",
              "X-Figma-Region": "figma_region",
            },
          };
        }

        if (item.slug === "github-http") {
          payload.secretBindings = {
            headers: {
              Authorization: "github_personal_access_token",
            },
          };
        }

        const installedPlugin = await installPlugin(payload);
        const testResult = await testPlugin(installedPlugin.pluginInstanceId);
        await loadData();
        setInfo(
          testResult.success
            ? `Plugin ${item.name} installato e testato (${testResult.toolCount} tool).`
            : `Plugin ${item.name} installato, ma test fallito: ${testResult.error ?? "errore sconosciuto"}.`,
        );
      } catch (installError) {
        setError(installError instanceof Error ? installError.message : "Installazione plugin fallita.");
      } finally {
        setBusyKey(null);
      }
    },
    [catalogReleaseChoice, installScope, installedSlugSet, loadData, promptDialog],
  );

  const handleIntegrateMcp = useCallback(async () => {
    try {
      setError(null);
      setInfo(null);

      const slug = await promptDialog(
        "Slug (es. 'my-mcp-http' o 'my-mcp-stdio')",
        "",
        "Integra MCP → Catalogo (bozza)",
      );
      if (!slug?.trim()) return;

      const name = await promptDialog("Nome visualizzato", "", "Integra MCP");
      if (!name?.trim()) return;

      const transportRaw = await promptDialog("Transport: 'http' oppure 'stdio'", "http", "Integra MCP");
      if (!transportRaw?.trim()) return;
      const transport = transportRaw.trim().toLowerCase() === "stdio" ? "stdio" : "http";

      const payload: IntegratePluginDraftPayload = {
        slug: slug.trim(),
        name: name.trim(),
        transport,
        description: "",
        defaultScope: "global",
      };

      if (transport === "http") {
        const httpUrl = await promptDialog(
          "URL del server MCP (es. https://example.com/mcp)",
          "",
          "Integra MCP (HTTP)",
        );
        if (!httpUrl?.trim()) return;
        payload.httpUrl = httpUrl.trim();
      } else {
        const cmd = await promptDialog("Comando (es. npx, node, python)", "npx", "Integra MCP (stdio)");
        if (!cmd?.trim()) return;
        payload.stdioCommand = cmd.trim();

        const argsCsv = await promptDialog(
          "Args (CSV). Esempio: -y, @scope/pkg@latest, --flag",
          "",
          "Integra MCP (stdio)",
        );
        if (argsCsv?.trim()) {
          payload.stdioArgs = normalizeCsv(argsCsv);
        }
      }

      setBusyKey("integrate:draft");
      const draft = await draftPluginIntegration(payload);
      setIntegrateDraft(draft);

      const topTools = draft.discoveredTools.slice(0, 12).map((t) => `- ${t.name}`).join("\n");
      const ok = await confirmDialog(
        `Tool scoperti: ${draft.toolCount}\n\n${topTools}${draft.toolCount > 12 ? "\n- …" : ""}\n\nVuoi pubblicare nel catalogo?`,
        "Bozza creata",
      );
      if (!ok) return;

      setBusyKey("integrate:publish");
      const res = await publishPluginIntegration({
        item: draft.item,
        version: "1.0.0",
        changelog: "Integrated via admin wizard",
      });

      setInfo(`Pubblicato nel catalogo: ${res.slug} (${res.version}).`);
      setActiveTab("catalog");
      await loadData();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore integrazione MCP.");
    } finally {
      setBusyKey(null);
    }
  }, [confirmDialog, loadData, promptDialog]);

  const handleToggle = useCallback(
    async (plugin: PluginInstance) => {
      const busyId = `toggle:${plugin.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await togglePlugin(plugin.id, !plugin.enabled);
        await loadData();
      } catch (toggleError) {
        setError(toggleError instanceof Error ? toggleError.message : "Toggle plugin fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [loadData],
  );

  const handleTest = useCallback(
    async (plugin: PluginInstance) => {
      const busyId = `test:${plugin.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        const result = await testPlugin(plugin.id);
        await loadData();
        setInfo(
          result.success
            ? `${plugin.name}: test ok (${result.toolCount} tool).`
            : `${plugin.name}: test fallito (${result.error ?? "errore sconosciuto"}).`,
        );
      } catch (testError) {
        setError(testError instanceof Error ? testError.message : "Test plugin fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [loadData],
  );

  const handleUpdateVersion = useCallback(
    async (plugin: PluginInstance) => {
      const selectedVersion = instanceReleaseChoice[plugin.id] || plugin.version;
      if (!selectedVersion) {
        setError("Versione non selezionata.");
        return;
      }

      const busyId = `update:${plugin.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await updatePluginVersion(plugin.id, selectedVersion);
        const testResult = await testPlugin(plugin.id);
        await loadData();
        setInfo(
          testResult.success
            ? `${plugin.name} aggiornato a ${selectedVersion} e testato.`
            : `${plugin.name} aggiornato a ${selectedVersion}, ma test fallito: ${testResult.error ?? "errore sconosciuto"}.`,
        );
      } catch (updateError) {
        setError(updateError instanceof Error ? updateError.message : "Aggiornamento plugin fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [instanceReleaseChoice, loadData],
  );

  const handleRollback = useCallback(
    async (plugin: PluginInstance) => {
      const catalogItem = catalog.find((item) => item.id === plugin.catalogItemId || item.slug === plugin.slug);
      const versions = catalogItem?.releases.map((release) => release.version) ?? [];
      const currentIndex = versions.findIndex((version) => version === plugin.version);
      if (currentIndex < 0 || currentIndex === versions.length - 1) {
        setError(`Nessuna versione precedente disponibile per ${plugin.name}.`);
        return;
      }
      const rollbackVersion = versions[currentIndex + 1];
      const confirm = await confirmDialog(
        `Vuoi effettuare rollback di ${plugin.name} alla versione ${rollbackVersion}?`,
        "Conferma rollback plugin",
      );
      if (!confirm) return;

      setInstanceReleaseChoice((prev) => ({ ...prev, [plugin.id]: rollbackVersion }));
      setBusyKey(`rollback:${plugin.id}`);
      setError(null);
      setInfo(null);
      try {
        await updatePluginVersion(plugin.id, rollbackVersion);
        const testResult = await testPlugin(plugin.id);
        await loadData();
        setInfo(
          testResult.success
            ? `${plugin.name} rollback completato a ${rollbackVersion}.`
            : `${plugin.name} rollback completato ma test fallito: ${testResult.error ?? "errore sconosciuto"}.`,
        );
      } catch (rollbackError) {
        setError(rollbackError instanceof Error ? rollbackError.message : "Rollback plugin fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [catalog, confirmDialog, loadData],
  );

  const handleSavePolicy = useCallback(
    async (plugin: PluginInstance) => {
      const draft = policyDrafts[plugin.id];
      if (!draft) return;
      const payload: PluginToolPolicy = {
        mode: draft.mode,
        tools: normalizeCsv(draft.tools),
        blockedTools: normalizeCsv(draft.blockedTools),
      };
      const busyId = `policy:${plugin.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await updatePluginToolPolicy(plugin.id, payload);
        await loadData();
        setInfo(`Policy aggiornata per ${plugin.name}.`);
      } catch (policyError) {
        setError(policyError instanceof Error ? policyError.message : "Salvataggio policy fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [loadData, policyDrafts],
  );

  const handleSaveSecret = useCallback(
    async (key: string) => {
      const value = secretDrafts[key] ?? "";
      const busyId = `secret:${key}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await updateAdminSetting(key, value);
        await loadData();
        setSecretDrafts((prev) => {
          const next = { ...prev };
          delete next[key];
          return next;
        });
        setInfo(`Chiave ${key} salvata.`);
      } catch (saveError) {
        setError(saveError instanceof Error ? saveError.message : "Salvataggio chiave fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [loadData, secretDrafts],
  );

  const handleStartFigmaOAuth = useCallback(async () => {
    const busyId = "figma-oauth-connect";
    setBusyKey(busyId);
    setError(null);
    setInfo(null);
    try {
      const result = await startFigmaOAuth("/admin/settings/connectors");
      window.location.assign(result.url);
    } catch (oauthError) {
      setError(oauthError instanceof Error ? oauthError.message : "Avvio OAuth Figma fallito.");
      setBusyKey(null);
    }
  }, []);

  const handleToggleFigmaStdioFallback = useCallback(
    async (enabled: boolean) => {
      const busyId = "figma-stdio-toggle";
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await updateAdminSetting("figma_mcp_prefer_stdio", enabled ? "true" : "false");
        await loadData();
        setInfo(
          enabled
            ? "Fallback stdio Figma attivato: il test userà figma-developer-mcp locale."
            : "Fallback stdio Figma disattivato: il test userà endpoint HTTP remoto.",
        );
      } catch (toggleError) {
        setError(toggleError instanceof Error ? toggleError.message : "Aggiornamento fallback Figma fallito.");
      } finally {
        setBusyKey(null);
      }
    },
    [loadData],
  );

  const handleAddLegacyPreset = useCallback(
    async (entry: CatalogEntry) => {
      const key = `${entry.name.toLowerCase()}::${entry.transport}`;
      if (legacyServerKeys.has(key)) {
        setInfo(`Il preset ${entry.name} è già presente tra gli MCP legacy.`);
        return;
      }
      const busyId = `legacy-add:${entry.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await createMcpServer(toLegacyPayload(entry));
        await loadData();
        setInfo(`Preset ${entry.name} aggiunto come MCP legacy.`);
      } catch (legacyError) {
        setError(legacyError instanceof Error ? legacyError.message : "Creazione MCP legacy fallita.");
      } finally {
        setBusyKey(null);
      }
    },
    [legacyServerKeys, loadData],
  );

  const handleMigrateLegacy = useCallback(
    async (server: McpServer) => {
      const migratableSlug = detectLegacyMigratableSlug(server);
      if (!migratableSlug) {
        setError("Questo MCP legacy non è mappabile automaticamente a un plugin curato.");
        return;
      }
      const confirm = await confirmDialog(
        `Migrare "${server.name}" verso plugin "${migratableSlug}"?`,
        "Conferma migrazione MCP legacy",
      );
      if (!confirm) return;

      const busyId = `legacy-migrate:${server.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        const res = await migrateLegacyMcpServerToPlugin(server.id);
        await loadData();
        setInfo(
          res.alreadyMigrated
            ? `${server.name} era già migrato.`
            : res.linkedExisting
              ? `${server.name} collegato a plugin esistente (${res.slug ?? "n/a"}).`
              : `${server.name} migrato a plugin (${res.slug ?? "n/a"}).`,
        );
      } catch (migrateError) {
        setError(migrateError instanceof Error ? migrateError.message : "Migrazione legacy fallita.");
      } finally {
        setBusyKey(null);
      }
    },
    [confirmDialog, loadData],
  );

  const handleUninstallPlugin = useCallback(
    async (plugin: PluginInstance) => {
      const confirm = await confirmDialog(
        `Disinstallare plugin "${plugin.name}"? L'adapter MCP collegato verrà rimosso.`,
        "Conferma disinstallazione plugin",
      );
      if (!confirm) return;
      const busyId = `plugin-uninstall:${plugin.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await uninstallPlugin(plugin.id);
        await loadData();
        setInfo(`Plugin ${plugin.name} disinstallato.`);
      } catch (uninstallError) {
        setError(uninstallError instanceof Error ? uninstallError.message : "Disinstallazione plugin fallita.");
      } finally {
        setBusyKey(null);
      }
    },
    [confirmDialog, loadData],
  );

  const handleDeleteLegacyMcp = useCallback(
    async (server: McpServer) => {
      const confirm = await confirmDialog(
        `Eliminare MCP legacy "${server.name}"?`,
        "Conferma eliminazione MCP legacy",
      );
      if (!confirm) return;
      const busyId = `legacy-delete:${server.id}`;
      setBusyKey(busyId);
      setError(null);
      setInfo(null);
      try {
        await deleteMcpServer(server.id);
        await loadData();
        setInfo(`MCP legacy ${server.name} eliminato.`);
      } catch (deleteError) {
        setError(deleteError instanceof Error ? deleteError.message : "Eliminazione MCP legacy fallita.");
      } finally {
        setBusyKey(null);
      }
    },
    [confirmDialog, loadData],
  );

  useEffect(() => {
    if (typeof window === "undefined") return;
    const params = new URLSearchParams(window.location.search);
    const status = params.get("figmaOauth");
    if (!status) return;

    const message = params.get("figmaMessage") ?? "";
    if (status === "ok") {
      setInfo(message || "OAuth Figma completato.");
      setError(null);
    } else {
      setError(message || "OAuth Figma fallito.");
    }

    params.delete("figmaOauth");
    params.delete("figmaMessage");
    const query = params.toString();
    const nextUrl = `${window.location.pathname}${query ? `?${query}` : ""}${window.location.hash}`;
    window.history.replaceState({}, "", nextUrl);

    void loadData();
  }, [loadData]);

  const tabs: Array<{ id: ManagerTab; label: string }> = [
    { id: "installed", label: "Installati" },
    { id: "catalog", label: "Catalogo" },
    { id: "policy", label: "Policy" },
    { id: "legacy", label: "Legacy MCP" },
  ];

  if (loading) {
    return <div className="text-base text-muted">Caricamento Plugin Manager...</div>;
  }

  return (
    <div>
      <div style={{ marginBottom: 12 }}>
        <div className="text-xl font-bold">Plugin Manager (MCP)</div>
        <div className="text-sm text-muted" style={{ marginTop: 4 }}>
          Catalogo curato, versioni pin, policy tool, health test, migrazione MCP legacy e gestione chiavi.
        </div>
      </div>

      <div className="card-sm flex-col-gap-8" style={{ marginBottom: 14 }}>
        <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
          <div className="text-base font-bold">
            Figma MCP OAuth
          </div>
          <span
            style={{
              fontSize: 10,
              padding: "2px 6px",
              borderRadius: 999,
              border: `1px solid ${figmaOAuthStatus?.configured ? "#22c55e66" : tc.warning}`,
              color: figmaOAuthStatus?.configured ? "#16a34a" : tc.warning,
              textTransform: "uppercase",
              fontWeight: 700,
            }}
          >
            {figmaOAuthStatus?.configured ? "client ok" : "client mancante"}
          </span>
          <span
            style={{
              fontSize: 10,
              padding: "2px 6px",
              borderRadius: 999,
              border: `1px solid ${figmaOAuthStatus?.hasAccessToken ? "#22c55e66" : tc.border}`,
              color: figmaOAuthStatus?.hasAccessToken ? "#16a34a" : tc.textMuted,
              textTransform: "uppercase",
              fontWeight: 700,
            }}
          >
            {figmaOAuthStatus?.hasAccessToken
              ? `token ${figmaOAuthStatus.tokenType === "pat" ? "PAT" : "OAuth"}`
              : "token assente"}
          </span>
          {figmaOAuthStatus?.tokenScope && (
            <span style={{ fontSize: 11, color: tc.textMuted }}>
              scope: {figmaOAuthStatus.tokenScope}
            </span>
          )}
        </div>
        <div className="text-sm text-muted">
          Se usi Figma MCP remoto serve OAuth con scope <code>mcp:connect</code>. In alternativa puoi usare il fallback
          locale <code>figma-developer-mcp</code>.
        </div>
        {figmaOAuthStatus?.lastError && (
          <div className="text-sm" style={{ color: tc.error }}>
            Ultimo errore OAuth: {figmaOAuthStatus.lastError}
          </div>
        )}
        <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
          <button
            type="button"
            onClick={() => void handleStartFigmaOAuth()}
            disabled={busyKey === "figma-oauth-connect" || !(figmaOAuthStatus?.configured ?? false)}
            className="btn btn-primary"
            style={actionButtonStyle(
              tc,
              busyKey === "figma-oauth-connect" || !(figmaOAuthStatus?.configured ?? false),
            )}
            title={
              figmaOAuthStatus?.configured
                ? "Connetti Figma con OAuth (mcp:connect)"
                : "Configura prima figma_client_id e figma_client_secret"
            }
          >
            {busyKey === "figma-oauth-connect" ? "Avvio OAuth..." : "Connetti OAuth"}
          </button>
          <button
            type="button"
            onClick={() => void handleToggleFigmaStdioFallback(!figmaPreferStdio)}
            disabled={busyKey === "figma-stdio-toggle"}
            style={actionButtonStyle(tc, busyKey === "figma-stdio-toggle")}
            title="Abilita/disabilita fallback stdio Figma"
          >
            {busyKey === "figma-stdio-toggle"
              ? "Aggiorno..."
              : figmaPreferStdio
                ? "Fallback stdio: attivo"
                : "Fallback stdio: disattivo"}
          </button>
          {figmaOAuthStatus?.redirectUri && (
            <span style={{ fontSize: 11, color: tc.textMuted }}>
              callback: {figmaOAuthStatus.redirectUri}
            </span>
          )}
        </div>
      </div>

      {/* Nota UX: le chiavi richieste ora sono richieste nel contesto del singolo plugin (tab "Installati"),
          così l'utente non deve cercarle in alto. */}

      {(error || info) && (
        <div className="text-sm" style={{
            marginBottom: 12,
            padding: "8px 10px",
            borderRadius: 8,
            border: `1px solid ${error ? tc.error : "#22c55e55"}`,
            background: error ? `${tc.error}14` : "#22c55e12",
            color: error ? tc.error : "#16a34a",
            whiteSpace: "pre-wrap",
          }}>
          {error ?? info}
        </div>
      )}

      <div className="flex-row" style={{ gap: 8, marginBottom: 14 }}>
        <div className="flex-row" style={{ gap: 8, flexWrap: "wrap" }}>
          {tabs.map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setActiveTab(tab.id)}
              className="text-sm font-semibold"
              style={{
                borderRadius: 999,
                border: `1px solid ${activeTab === tab.id ? tc.accent : tc.border}`,
                background: activeTab === tab.id ? `${tc.accent}15` : tc.bgInput,
                color: activeTab === tab.id ? tc.accent : tc.textMuted,
                padding: "5px 12px",
                cursor: "pointer",
              }}
            >
              {tab.label}
            </button>
          ))}
        </div>
        <div style={{ flex: 1 }} />
        <button
          type="button"
          onClick={() => void handleIntegrateMcp()}
          disabled={busyKey === "integrate:draft" || busyKey === "integrate:publish"}
          className="text-sm font-semibold"
          style={{
            borderRadius: 999,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: tc.text,
            padding: "5px 12px",
            cursor: "pointer",
            opacity: busyKey === "integrate:draft" || busyKey === "integrate:publish" ? 0.7 : 1,
          }}
          title="Crea una entry nel catalogo via tool discovery"
        >
          {busyKey === "integrate:draft"
            ? "Integro…"
            : busyKey === "integrate:publish"
              ? "Pubblico…"
              : "+ Integra MCP"}
        </button>
      </div>

      {activeTab === "installed" && (
        <div style={{ display: "grid", gap: 10 }}>
          {installed.length === 0 && (
            <div className="text-sm text-muted">
              Nessun plugin installato.
            </div>
          )}
          {installed.map((plugin) => {
            const catalogItem = catalogById.get(plugin.catalogItemId ?? "");
            const releases = catalogItem?.releases ?? [];
            const selectedVersion = instanceReleaseChoice[plugin.id] || plugin.version || "";
            const isBusy = busyKey?.includes(plugin.id) ?? false;
            const requiredKeys = (catalogItem?.requiredSecretRefs ?? []).map((k) => k.trim()).filter(Boolean);
            const missingKeys = requiredKeys.filter((key) => !(settingsByKey.get(key)?.has_value ?? false));
            return (
              <div
                key={plugin.id}
                className="card-sm"
                style={{
                  opacity: plugin.enabled ? 1 : 0.72,
                }}
              >
                <div className="flex-row" style={{ gap: 8, marginBottom: 5 }}>
                  <span
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: "50%",
                      background: healthColor(plugin.healthStatus),
                    }}
                  />
                  <div className="text-lg font-bold">{plugin.name}</div>
                  <span
                    style={{
                      fontSize: 10,
                      padding: "2px 6px",
                      borderRadius: 999,
                      border: `1px solid ${tc.border}`,
                      color: tc.textMuted,
                      textTransform: "uppercase",
                    }}
                  >
                    {plugin.transport}
                  </span>
                  <span
                    style={{
                      fontSize: 10,
                      padding: "2px 6px",
                      borderRadius: 999,
                      border: `1px solid ${tc.border}`,
                      color: tc.textMuted,
                      textTransform: "uppercase",
                    }}
                  >
                    {plugin.scope}
                  </span>
                </div>
                <div className="text-sm text-muted" style={{ marginBottom: 8 }}>
                  {plugin.catalogDescription}
                </div>
                <div className="text-xs text-muted" style={{ marginBottom: 10 }}>
                  Versione: {plugin.version ?? "n/a"} · Health: {plugin.healthStatus}
                  {plugin.lastHealthMessage ? ` · ${plugin.lastHealthMessage}` : ""}
                </div>

                {missingKeys.length > 0 && plugin.canManage && (
                  <div
                    style={{
                      border: `1px solid ${tc.warning}`,
                      background: `${tc.warning}10`,
                      borderRadius: 10,
                      padding: "10px 12px",
                      marginBottom: 10,
                      display: "grid",
                      gap: 8,
                    }}
                  >
                    <div style={{ fontWeight: 700, fontSize: 12, color: tc.warning }}>
                      Chiavi mancanti per usare questo MCP
                    </div>
                    <div style={{ fontSize: 12, color: tc.textMuted }}>
                      Inserisci le chiavi richieste qui sotto. Sono salvate lato server e non vengono restituite in chiaro.
                    </div>
                    {missingKeys.map((key) => (
                      <div key={key} style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                        <span style={{ fontFamily: "monospace", fontSize: 12, color: tc.text, minWidth: 220 }}>
                          {key}
                        </span>
                        <input
                          type="password"
                          value={secretDrafts[key] ?? ""}
                          placeholder="Inserisci valore segreto"
                          onChange={(event) => setSecretDrafts((prev) => ({ ...prev, [key]: event.target.value }))}
                          style={inputStyle(tc)}
                        />
                        <button
                          type="button"
                          onClick={() => void handleSaveSecret(key)}
                          disabled={busyKey === `secret:${key}` || !(secretDrafts[key] ?? "").trim()}
                          style={actionButtonStyle(tc, busyKey === `secret:${key}` || !(secretDrafts[key] ?? "").trim())}
                        >
                          {busyKey === `secret:${key}` ? "Salvo..." : "Salva chiave"}
                        </button>
                      </div>
                    ))}
                  </div>
                )}

                <div className="flex-row" style={{ flexWrap: "wrap", gap: 8 }}>
                  <button
                    type="button"
                    disabled={isBusy || !plugin.canManage}
                    onClick={() => void handleToggle(plugin)}
                    style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
                  >
                    {plugin.enabled ? "Disabilita" : "Abilita"}
                  </button>
                  <button
                    type="button"
                    disabled={isBusy || !plugin.canManage}
                    onClick={() => void handleTest(plugin)}
                    style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
                  >
                    Test
                  </button>
                  <select
                    value={selectedVersion}
                    disabled={!plugin.canManage || releases.length === 0}
                    onChange={(event) =>
                      setInstanceReleaseChoice((prev) => ({ ...prev, [plugin.id]: event.target.value }))
                    }
                    style={selectStyle(tc, 180)}
                  >
                    {releases.length === 0 && <option value="">Nessun rilascio</option>}
                    {releases.map((release) => (
                      <option key={release.version} value={release.version}>
                        {release.version}
                      </option>
                    ))}
                  </select>
                  <button
                    type="button"
                    disabled={isBusy || !plugin.canManage || !selectedVersion}
                    onClick={() => void handleUpdateVersion(plugin)}
                    style={actionButtonStyle(tc, isBusy || !plugin.canManage || !selectedVersion)}
                  >
                    Aggiorna
                  </button>
                  <button
                    type="button"
                    disabled={isBusy || !plugin.canManage}
                    onClick={() => void handleRollback(plugin)}
                    style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
                  >
                    Rollback
                  </button>
                  <button
                    type="button"
                    disabled={isBusy || !plugin.canManage}
                    onClick={() => void handleUninstallPlugin(plugin)}
                    style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
                  >
                    Disinstalla
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {activeTab === "catalog" && (
        <div style={{ display: "grid", gap: 14 }}>
          <div style={{ display: "flex", gap: 10, marginBottom: 4, alignItems: "center" }}>
            <input
              value={catalogSearch}
              onChange={(event) => setCatalogSearch(event.target.value)}
              placeholder="Cerca plugin/preset MCP..."
              style={inputStyle(tc)}
            />
            <select
              value={installScope}
              onChange={(event) => setInstallScope(event.target.value as "global" | "project" | "user")}
              style={selectStyle(tc)}
            >
              <option value="global">Scope globale</option>
              <option value="project">Scope progetto</option>
              <option value="user">Scope utente</option>
            </select>
            <button
              type="button"
              onClick={() => setShowAlreadyPresent((v) => !v)}
              style={{
                border: `1px solid ${tc.border}`,
                background: showAlreadyPresent ? `${tc.accent}15` : tc.bgInput,
                color: showAlreadyPresent ? tc.accent : tc.textMuted,
                borderRadius: 999,
                padding: "5px 10px",
                fontSize: 12,
                cursor: "pointer",
                fontWeight: 600,
                whiteSpace: "nowrap",
              }}
              title="Mostra/nasconde elementi già presenti (installati o già aggiunti come legacy)"
            >
              {showAlreadyPresent ? "Mostra: tutti" : "Mostra: non presenti"}
            </button>
          </div>

          <div
            style={{
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
              background: tc.bgCard,
              padding: "10px 12px",
              display: "grid",
              gap: 10,
            }}
          >
            <div style={{ fontWeight: 700, fontSize: 13, color: tc.text }}>
              Catalogo plugin curato (installazione plugin)
            </div>
            <div style={{ display: "grid", gap: 10 }}>
              {visibleCuratedCatalog.map((item) => {
                const selectedVersion = catalogReleaseChoice[item.id] || defaultReleaseVersion(item);
                const alreadyInstalled = installedSlugSet.has(item.slug.toLowerCase());
                const missingSecrets = item.requiredSecretRefs.filter((key) => {
                  const setting = settingsByKey.get(key);
                  return !(setting?.has_value ?? false);
                });
                const cannotInstall = !item.isAllowlisted || alreadyInstalled;
                return (
                  <div
                    key={item.id}
                    style={{
                      border: `1px solid ${alreadyInstalled ? "#f59e0b66" : tc.border}`,
                      borderRadius: 10,
                      background: alreadyInstalled ? "#f59e0b12" : tc.bgCard,
                      padding: "10px 12px",
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                      <div style={{ fontWeight: 700, color: tc.text }}>{item.name}</div>
                      <span
                        style={{
                          fontSize: 10,
                          padding: "2px 6px",
                          borderRadius: 999,
                          border: `1px solid ${item.isAllowlisted ? "#22c55e66" : tc.error}`,
                          color: item.isAllowlisted ? "#16a34a" : tc.error,
                          textTransform: "uppercase",
                        }}
                      >
                        {item.isAllowlisted ? "allowlisted" : "blocked"}
                      </span>
                      {alreadyInstalled && (
                        <span
                          style={{
                            fontSize: 10,
                            padding: "2px 6px",
                            borderRadius: 999,
                            border: "1px solid #f59e0b66",
                            color: "#b45309",
                            textTransform: "uppercase",
                          }}
                          title="Plugin già installato: reinstallazione bloccata"
                        >
                          duplica bloccata
                        </span>
                      )}
                      <span style={{ fontSize: 11, color: tc.textMuted }}>{item.slug}</span>
                    </div>
                    <div style={{ fontSize: 12, color: tc.textMuted, marginBottom: 6 }}>{item.description}</div>
                    <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8 }}>
                      Secret richiesti:{" "}
                      {item.requiredSecretRefs.length > 0 ? item.requiredSecretRefs.join(", ") : "nessuno"}
                    </div>
                    {missingSecrets.length > 0 && (
                      <div style={{ fontSize: 11, color: tc.warning, marginBottom: 8 }}>
                        Mancano chiavi: {missingSecrets.join(", ")}
                      </div>
                    )}
                    {alreadyInstalled && (
                      <div style={{ fontSize: 11, color: "#b45309", marginBottom: 8 }}>
                        Questo plugin è già presente negli installati. Reinstallazione non consentita.
                      </div>
                    )}
                    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                      <select
                        value={selectedVersion}
                        onChange={(event) =>
                          setCatalogReleaseChoice((prev) => ({ ...prev, [item.id]: event.target.value }))
                        }
                        style={selectStyle(tc, 170)}
                      >
                        {item.releases.map((release) => (
                          <option key={release.version} value={release.version}>
                            {release.version}
                          </option>
                        ))}
                      </select>
                      <button
                        type="button"
                        disabled={busyKey === `install:${item.id}` || cannotInstall}
                        onClick={() => void handleInstall(item)}
                        style={actionButtonStyle(tc, busyKey === `install:${item.id}` || cannotInstall)}
                        title={
                          alreadyInstalled
                            ? "Plugin già installato: reinstallazione bloccata"
                            : !item.isAllowlisted
                              ? "Plugin non allowlisted"
                              : "Installa plugin"
                        }
                      >
                        {alreadyInstalled
                          ? "Già installato"
                          : busyKey === `install:${item.id}`
                            ? "Installo..."
                            : "Installa"}
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          <div
            style={{
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
              background: tc.bgCard,
              padding: "10px 12px",
              display: "grid",
              gap: 10,
            }}
          >
            <div style={{ fontWeight: 700, fontSize: 13, color: tc.text }}>
              Catalogo MCP esteso (preset legacy)
            </div>
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Se un plugin non è nel catalogo curato, puoi aggiungerlo come MCP legacy e poi migrarlo quando disponibile.
            </div>
            <div style={{ display: "grid", gap: 8 }}>
              {visibleLegacyPresetCatalog.map((entry) => {
                const legacyKey = `${entry.name.toLowerCase()}::${entry.transport}`;
                const alreadyAdded = legacyServerKeys.has(legacyKey);
                return (
                  <div
                    key={entry.id}
                    style={{
                      border: `1px solid ${alreadyAdded ? "#f59e0b66" : tc.border}`,
                      borderRadius: 10,
                      background: alreadyAdded ? "#f59e0b12" : tc.bgCard,
                      padding: "10px 12px",
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                      <span style={{ fontSize: 16 }}>{entry.icon}</span>
                      <div style={{ fontWeight: 700, color: tc.text }}>{entry.name}</div>
                      <span
                        style={{
                          fontSize: 10,
                          padding: "2px 6px",
                          borderRadius: 999,
                          border: `1px solid ${tc.border}`,
                          color: tc.textMuted,
                          textTransform: "uppercase",
                        }}
                      >
                        {entry.transport}
                      </span>
                    </div>
                    <div style={{ fontSize: 12, color: tc.textMuted, marginBottom: 8 }}>
                      {entry.description}
                    </div>
                    <button
                      type="button"
                      disabled={busyKey === `legacy-add:${entry.id}` || alreadyAdded}
                      onClick={() => void handleAddLegacyPreset(entry)}
                      style={actionButtonStyle(
                        tc,
                        busyKey === `legacy-add:${entry.id}` || alreadyAdded,
                      )}
                    >
                      {alreadyAdded
                        ? "Già presente"
                        : busyKey === `legacy-add:${entry.id}`
                          ? "Aggiungo..."
                          : "Aggiungi come legacy MCP"}
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      )}

      {activeTab === "policy" && (
        <div style={{ display: "grid", gap: 10 }}>
          {installed.length === 0 && (
            <div style={{ color: tc.textMuted, fontSize: 12 }}>Nessun plugin installato.</div>
          )}
          {installed.map((plugin) => {
            const draft = policyDrafts[plugin.id] ?? {
              mode: "all" as const,
              tools: "",
              blockedTools: "",
            };
            return (
              <div
                key={plugin.id}
                style={{
                  border: `1px solid ${tc.border}`,
                  borderRadius: 10,
                  background: tc.bgCard,
                  padding: "12px 14px",
                }}
              >
                <div style={{ fontWeight: 700, fontSize: 13, color: tc.text, marginBottom: 8 }}>
                  {plugin.name}
                </div>
                <div style={{ display: "grid", gap: 8 }}>
                  <select
                    value={draft.mode}
                    disabled={!plugin.canManage}
                    onChange={(event) =>
                      setPolicyDrafts((prev) => ({
                        ...prev,
                        [plugin.id]: { ...draft, mode: event.target.value as PluginToolPolicy["mode"] },
                      }))
                    }
                    style={selectStyle(tc)}
                  >
                    <option value="all">All</option>
                    <option value="allowlist">Allowlist</option>
                    <option value="denylist">Denylist</option>
                  </select>
                  <input
                    value={draft.tools}
                    disabled={!plugin.canManage}
                    onChange={(event) =>
                      setPolicyDrafts((prev) => ({
                        ...prev,
                        [plugin.id]: { ...draft, tools: event.target.value },
                      }))
                    }
                    placeholder="Tool consentiti (CSV)"
                    style={inputStyle(tc)}
                  />
                  <input
                    value={draft.blockedTools}
                    disabled={!plugin.canManage}
                    onChange={(event) =>
                      setPolicyDrafts((prev) => ({
                        ...prev,
                        [plugin.id]: { ...draft, blockedTools: event.target.value },
                      }))
                    }
                    placeholder="Tool bloccati (CSV)"
                    style={inputStyle(tc)}
                  />
                  <div>
                    <button
                      type="button"
                      disabled={!plugin.canManage || busyKey === `policy:${plugin.id}`}
                      onClick={() => void handleSavePolicy(plugin)}
                      style={actionButtonStyle(tc, !plugin.canManage || busyKey === `policy:${plugin.id}`)}
                    >
                      {busyKey === `policy:${plugin.id}` ? "Salvo..." : "Salva policy"}
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {activeTab === "legacy" && (
        <div style={{ display: "grid", gap: 10 }}>
          <div style={{ fontSize: 12, color: tc.textMuted }}>
            MCP legacy rilevati. Se mappabili, puoi migrarli in plugin con un click.
          </div>
          {legacyConnectors.length === 0 && (
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Nessun connettore legacy rilevato.
            </div>
          )}
          {legacyConnectors.map((server) => {
            const slug = detectLegacyMigratableSlug(server);
            const isBuiltin = (server.transport as string) === "builtin";
            return (
              <div
                key={server.id}
                style={{
                  border: `1px solid ${tc.border}`,
                  borderRadius: 10,
                  background: tc.bgCard,
                  padding: "10px 12px",
                }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <div style={{ fontWeight: 700, fontSize: 13, color: tc.text }}>{server.name}</div>
                  <span
                    style={{
                      fontSize: 10,
                      border: `1px solid ${tc.border}`,
                      borderRadius: 999,
                      padding: "2px 6px",
                      color: tc.textMuted,
                      textTransform: "uppercase",
                    }}
                  >
                    {server.transport}
                  </span>
                  <span style={{ fontSize: 11, color: tc.textMuted }}>
                    {server.enabled ? "enabled" : "disabled"}
                  </span>
                  {isBuiltin && (
                    <span
                      style={{
                        fontSize: 10,
                        border: "1px solid #22c55e66",
                        borderRadius: 999,
                        padding: "2px 6px",
                        color: "#16a34a",
                        textTransform: "uppercase",
                      }}
                      title="MCP integrato nel core Nexus (non è un legacy da migrare)"
                    >
                      integrato
                    </span>
                  )}
                  {slug && (
                    <span
                      style={{
                        fontSize: 10,
                        border: "1px solid #22c55e66",
                        borderRadius: 999,
                        padding: "2px 6px",
                        color: "#16a34a",
                        textTransform: "uppercase",
                      }}
                    >
                      migrabile → {slug}
                    </span>
                  )}
                </div>
                <div style={{ marginTop: 4, fontSize: 11, color: tc.textMuted }}>
                  {server.transport === "http"
                    ? server.url
                    : `${server.command ?? ""} ${(server.args ?? []).join(" ")}`.trim()}
                </div>
                <div style={{ marginTop: 8 }}>
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" }}>
                    <button
                      type="button"
                      disabled={isBuiltin || !slug || busyKey === `legacy-migrate:${server.id}`}
                      onClick={() => void handleMigrateLegacy(server)}
                      style={actionButtonStyle(
                        tc,
                        isBuiltin || !slug || busyKey === `legacy-migrate:${server.id}`,
                      )}
                    >
                      {isBuiltin
                        ? "Già integrato"
                        : !slug
                          ? "Non migrabile automaticamente"
                        : busyKey === `legacy-migrate:${server.id}`
                          ? "Migrazione..."
                          : "Migra a plugin"}
                    </button>
                    <button
                      type="button"
                      disabled={isBuiltin || busyKey === `legacy-delete:${server.id}`}
                      onClick={() => void handleDeleteLegacyMcp(server)}
                      style={actionButtonStyle(tc, isBuiltin || busyKey === `legacy-delete:${server.id}`)}
                    >
                      {isBuiltin ? "Integrato" : busyKey === `legacy-delete:${server.id}` ? "Elimino..." : "Elimina MCP"}
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function actionButtonStyle(
  tc: ReturnType<typeof useThemeColors>,
  disabled = false,
): React.CSSProperties {
  return {
    border: `1px solid ${tc.accent}`,
    background: `${tc.accent}15`,
    color: tc.accent,
    borderRadius: 7,
    padding: "5px 10px",
    fontSize: 12,
    cursor: disabled ? "not-allowed" : "pointer",
    fontWeight: 600,
    opacity: disabled ? 0.55 : 1,
  };
}

function inputStyle(tc: ReturnType<typeof useThemeColors>): React.CSSProperties {
  return {
    width: "100%",
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    borderRadius: 8,
    padding: "7px 10px",
    fontSize: 12,
    boxSizing: "border-box",
  };
}

function selectStyle(
  tc: ReturnType<typeof useThemeColors>,
  width = 150,
): React.CSSProperties {
  return {
    minWidth: width,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    borderRadius: 8,
    padding: "6px 8px",
    fontSize: 12,
  };
}
