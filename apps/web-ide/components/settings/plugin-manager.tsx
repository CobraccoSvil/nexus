"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  useProjectStore,
  selectPluginsChangedAt,
} from "../../lib/project-dispatcher/store";
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
import { CatalogTab } from "./plugin-manager/catalog-tab";
import { FigmaOAuthCard } from "./plugin-manager/figma-oauth-card";
import { InstalledPluginsList } from "./plugin-manager/installed-plugins-list";
import { LegacyMcpList } from "./plugin-manager/legacy-mcp-list";
import {
  dedupeInstalledPlugins,
  defaultReleaseVersion,
  detectLegacyMigratableSlug,
  normalizeCsv,
  toLegacyPayload,
} from "./plugin-manager/plugin-helpers";
import { PolicyTab } from "./plugin-manager/policy-tab";
import type { ManagerTab, PluginTestStatus, PolicyDraft } from "./plugin-manager/types";

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
  const [testStatusByPluginId, setTestStatusByPluginId] = useState<
    Record<string, PluginTestStatus>
  >({});
  const [catalogSearch, setCatalogSearch] = useState("");
  const [installScope, setInstallScope] = useState<"global" | "project" | "user">("global");
  const [showAlreadyPresent, setShowAlreadyPresent] = useState(false);
  const [catalogReleaseChoice, setCatalogReleaseChoice] = useState<Record<string, string>>({});
  const [instanceReleaseChoice, setInstanceReleaseChoice] = useState<Record<string, string>>({});
  const [policyDrafts, setPolicyDrafts] = useState<Record<string, PolicyDraft>>({});
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});
  const [, setIntegrateDraft] = useState<IntegratePluginDraftResult | null>(null);

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

  // Auto-refresh quando un plugin viene installato/rimosso/abilitato via dispatcher SSE
  const pluginsChangedAt = useProjectStore(selectPluginsChangedAt);
  useEffect(() => {
    if (pluginsChangedAt > 0) void loadData();
  }, [pluginsChangedAt, loadData]);

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
        setTestStatusByPluginId((prev) => ({
          ...prev,
          [installedPlugin.pluginInstanceId]: {
            success: testResult.success,
            toolCount: testResult.toolCount,
            error: testResult.error,
            at: new Date().toISOString(),
          },
        }));
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
        setTestStatusByPluginId((prev) => ({
          ...prev,
          [plugin.id]: {
            success: result.success,
            toolCount: result.toolCount,
            error: result.error,
            at: new Date().toISOString(),
          },
        }));
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
        setTestStatusByPluginId((prev) => ({
          ...prev,
          [plugin.id]: {
            success: testResult.success,
            toolCount: testResult.toolCount,
            error: testResult.error,
            at: new Date().toISOString(),
          },
        }));
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
        setTestStatusByPluginId((prev) => ({
          ...prev,
          [plugin.id]: {
            success: testResult.success,
            toolCount: testResult.toolCount,
            error: testResult.error,
            at: new Date().toISOString(),
          },
        }));
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

      <FigmaOAuthCard
        tc={tc}
        figmaOAuthStatus={figmaOAuthStatus}
        figmaPreferStdio={figmaPreferStdio}
        busyKey={busyKey}
        onStartOAuth={() => void handleStartFigmaOAuth()}
        onToggleStdioFallback={(enabled) => void handleToggleFigmaStdioFallback(enabled)}
      />

      {/* Nota UX: le chiavi richieste ora sono richieste nel contesto del singolo plugin (tab "Installati"),
          così l'utente non deve cercarle in alto. */}

      {(error || info) && (
        <div
          style={{
            position: "sticky",
            top: 8,
            zIndex: 20,
            marginBottom: 12,
          }}
        >
          <div
            className="text-sm"
            style={{
              padding: "8px 10px",
              borderRadius: 8,
              border: `1px solid ${error ? tc.error : "#22c55e55"}`,
              background: error ? `${tc.error}14` : "#22c55e12",
              color: error ? tc.error : "#16a34a",
              whiteSpace: "pre-wrap",
              boxShadow: "0 8px 24px rgba(0,0,0,0.08)",
              backdropFilter: "blur(6px)",
            }}
          >
            {error ?? info}
          </div>
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
        <InstalledPluginsList
          tc={tc}
          installed={installed}
          catalogById={catalogById}
          instanceReleaseChoice={instanceReleaseChoice}
          settingsByKey={settingsByKey}
          testStatusByPluginId={testStatusByPluginId}
          secretDrafts={secretDrafts}
          busyKey={busyKey}
          onInstanceReleaseChoiceChange={(pluginId, version) =>
            setInstanceReleaseChoice((prev) => ({ ...prev, [pluginId]: version }))
          }
          onSecretDraftChange={(key, value) => setSecretDrafts((prev) => ({ ...prev, [key]: value }))}
          onSaveSecret={(key) => void handleSaveSecret(key)}
          onToggle={(plugin) => void handleToggle(plugin)}
          onTest={(plugin) => void handleTest(plugin)}
          onUpdateVersion={(plugin) => void handleUpdateVersion(plugin)}
          onRollback={(plugin) => void handleRollback(plugin)}
          onUninstall={(plugin) => void handleUninstallPlugin(plugin)}
        />
      )}

      {activeTab === "catalog" && (
        <CatalogTab
          tc={tc}
          catalogSearch={catalogSearch}
          installScope={installScope}
          showAlreadyPresent={showAlreadyPresent}
          visibleCuratedCatalog={visibleCuratedCatalog}
          visibleLegacyPresetCatalog={visibleLegacyPresetCatalog}
          installedSlugSet={installedSlugSet}
          legacyServerKeys={legacyServerKeys}
          settingsByKey={settingsByKey}
          catalogReleaseChoice={catalogReleaseChoice}
          busyKey={busyKey}
          onCatalogSearchChange={setCatalogSearch}
          onInstallScopeChange={setInstallScope}
          onToggleShowAlreadyPresent={() => setShowAlreadyPresent((v) => !v)}
          onCatalogReleaseChoiceChange={(itemId, version) =>
            setCatalogReleaseChoice((prev) => ({ ...prev, [itemId]: version }))
          }
          onInstall={(item) => void handleInstall(item)}
          onAddLegacyPreset={(entry) => void handleAddLegacyPreset(entry)}
        />
      )}

      {activeTab === "policy" && (
        <PolicyTab
          tc={tc}
          installed={installed}
          policyDrafts={policyDrafts}
          busyKey={busyKey}
          onPolicyDraftChange={(pluginId, draft) =>
            setPolicyDrafts((prev) => ({ ...prev, [pluginId]: draft }))
          }
          onSavePolicy={(plugin) => void handleSavePolicy(plugin)}
        />
      )}

      {activeTab === "legacy" && (
        <LegacyMcpList
          tc={tc}
          legacyConnectors={legacyConnectors}
          busyKey={busyKey}
          onMigrate={(server) => void handleMigrateLegacy(server)}
          onDelete={(server) => void handleDeleteLegacyMcp(server)}
        />
      )}
    </div>
  );
}
