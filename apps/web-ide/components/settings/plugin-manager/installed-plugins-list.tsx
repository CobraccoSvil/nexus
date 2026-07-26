"use client";

import type {
  AdminSettingEntry,
  PluginCatalogItem,
  PluginInstance,
} from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";
import { AutoWidthSelect } from "../../auto-width-select";
import { healthColor } from "./plugin-helpers";
import { actionButtonStyle, inputStyle, selectStyle } from "./plugin-styles";
import type { PluginTestStatus } from "./types";

interface InstalledPluginsListProps {
  tc: Theme;
  installed: PluginInstance[];
  catalogById: Map<string, PluginCatalogItem>;
  instanceReleaseChoice: Record<string, string>;
  settingsByKey: Map<string, AdminSettingEntry>;
  testStatusByPluginId: Record<string, PluginTestStatus>;
  secretDrafts: Record<string, string>;
  busyKey: string | null;
  onInstanceReleaseChoiceChange: (pluginId: string, version: string) => void;
  onSecretDraftChange: (key: string, value: string) => void;
  onSaveSecret: (key: string) => void;
  onToggle: (plugin: PluginInstance) => void;
  onTest: (plugin: PluginInstance) => void;
  onUpdateVersion: (plugin: PluginInstance) => void;
  onRollback: (plugin: PluginInstance) => void;
  onUninstall: (plugin: PluginInstance) => void;
}

export function InstalledPluginsList({
  tc,
  installed,
  catalogById,
  instanceReleaseChoice,
  settingsByKey,
  testStatusByPluginId,
  secretDrafts,
  busyKey,
  onInstanceReleaseChoiceChange,
  onSecretDraftChange,
  onSaveSecret,
  onToggle,
  onTest,
  onUpdateVersion,
  onRollback,
  onUninstall,
}: InstalledPluginsListProps) {
  return (
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
        // Senza rilasci la tendina mostrava una option segnaposto: la stessa
        // condizione ora costruisce l'array delle opzioni.
        const releaseOptions =
          releases.length === 0
            ? [{ value: "", label: "Nessun rilascio" }]
            : releases.map((release) => ({ value: release.version, label: release.version }));
        const isBusy = busyKey?.includes(plugin.id) ?? false;
        const requiredKeys = (catalogItem?.requiredSecretRefs ?? []).map((k) => k.trim()).filter(Boolean);
        const missingKeys = requiredKeys.filter((key) => !(settingsByKey.get(key)?.has_value ?? false));
        const lastTest = testStatusByPluginId[plugin.id];
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
            {lastTest && (
              <div
                className="text-xs"
                style={{
                  marginBottom: 10,
                  color: lastTest.success ? "#16a34a" : tc.error,
                  whiteSpace: "pre-wrap",
                }}
              >
                {lastTest.success
                  ? `Test: ok (${lastTest.toolCount} tool) · ${new Date(lastTest.at).toLocaleString()}`
                  : `Test: fallito (${lastTest.error ?? "errore sconosciuto"}) · ${new Date(lastTest.at).toLocaleString()}`}
              </div>
            )}

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
                    <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: tc.text, minWidth: 220 }}>
                      {key}
                    </span>
                    <input
                      type="password"
                      value={secretDrafts[key] ?? ""}
                      placeholder="Inserisci valore segreto"
                      onChange={(event) => onSecretDraftChange(key, event.target.value)}
                      style={inputStyle(tc)}
                    />
                    <button
                      type="button"
                      onClick={() => onSaveSecret(key)}
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
                onClick={() => onToggle(plugin)}
                style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
              >
                {plugin.enabled ? "Disabilita" : "Abilita"}
              </button>
              <button
                type="button"
                disabled={isBusy || !plugin.canManage}
                onClick={() => onTest(plugin)}
                style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
              >
                Test
              </button>
              <AutoWidthSelect
                value={selectedVersion}
                options={releaseOptions}
                disabled={!plugin.canManage || releases.length === 0}
                onChange={(value) => onInstanceReleaseChoiceChange(plugin.id, value)}
                style={selectStyle(tc, 180)}
              />
              <button
                type="button"
                disabled={isBusy || !plugin.canManage || !selectedVersion}
                onClick={() => onUpdateVersion(plugin)}
                style={actionButtonStyle(tc, isBusy || !plugin.canManage || !selectedVersion)}
              >
                Aggiorna
              </button>
              <button
                type="button"
                disabled={isBusy || !plugin.canManage}
                onClick={() => onRollback(plugin)}
                style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
              >
                Rollback
              </button>
              <button
                type="button"
                disabled={isBusy || !plugin.canManage}
                onClick={() => onUninstall(plugin)}
                style={actionButtonStyle(tc, isBusy || !plugin.canManage)}
              >
                Disinstalla
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
