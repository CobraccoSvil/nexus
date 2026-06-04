"use client";

import type { AdminSettingEntry, PluginCatalogItem } from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";
import type { CatalogEntry } from "../mcp-catalog-data";
import { defaultReleaseVersion } from "./plugin-helpers";
import { actionButtonStyle, inputStyle, selectStyle } from "./plugin-styles";

interface CatalogTabProps {
  tc: Theme;
  catalogSearch: string;
  installScope: "global" | "project" | "user";
  showAlreadyPresent: boolean;
  visibleCuratedCatalog: PluginCatalogItem[];
  visibleLegacyPresetCatalog: CatalogEntry[];
  installedSlugSet: Set<string>;
  legacyServerKeys: Set<string>;
  settingsByKey: Map<string, AdminSettingEntry>;
  catalogReleaseChoice: Record<string, string>;
  busyKey: string | null;
  onCatalogSearchChange: (value: string) => void;
  onInstallScopeChange: (value: "global" | "project" | "user") => void;
  onToggleShowAlreadyPresent: () => void;
  onCatalogReleaseChoiceChange: (itemId: string, version: string) => void;
  onInstall: (item: PluginCatalogItem) => void;
  onAddLegacyPreset: (entry: CatalogEntry) => void;
}

export function CatalogTab({
  tc,
  catalogSearch,
  installScope,
  showAlreadyPresent,
  visibleCuratedCatalog,
  visibleLegacyPresetCatalog,
  installedSlugSet,
  legacyServerKeys,
  settingsByKey,
  catalogReleaseChoice,
  busyKey,
  onCatalogSearchChange,
  onInstallScopeChange,
  onToggleShowAlreadyPresent,
  onCatalogReleaseChoiceChange,
  onInstall,
  onAddLegacyPreset,
}: CatalogTabProps) {
  return (
    <div style={{ display: "grid", gap: 14 }}>
      <div style={{ display: "flex", gap: 10, marginBottom: 4, alignItems: "center" }}>
        <input
          value={catalogSearch}
          onChange={(event) => onCatalogSearchChange(event.target.value)}
          placeholder="Cerca plugin/preset MCP..."
          style={inputStyle(tc)}
        />
        <select
          value={installScope}
          onChange={(event) => onInstallScopeChange(event.target.value as "global" | "project" | "user")}
          style={selectStyle(tc)}
        >
          <option value="global">Scope globale</option>
          <option value="project">Scope progetto</option>
          <option value="user">Scope utente</option>
        </select>
        <button
          type="button"
          onClick={() => onToggleShowAlreadyPresent()}
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
                    onChange={(event) => onCatalogReleaseChoiceChange(item.id, event.target.value)}
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
                    onClick={() => onInstall(item)}
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
                  onClick={() => onAddLegacyPreset(entry)}
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
  );
}
