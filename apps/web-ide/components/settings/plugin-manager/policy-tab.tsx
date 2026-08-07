"use client";

import type { PluginInstance, PluginToolPolicy } from "../../../lib/api-client";
import type { Theme } from "../../../lib/theme";
import { actionButtonStyle, inputStyle, selectStyle } from "./plugin-styles";
import type { PolicyDraft } from "./types";
import { useI18n } from "../../../lib/i18n";

interface PolicyTabProps {
  tc: Theme;
  installed: PluginInstance[];
  policyDrafts: Record<string, PolicyDraft>;
  busyKey: string | null;
  onPolicyDraftChange: (pluginId: string, draft: PolicyDraft) => void;
  onSavePolicy: (plugin: PluginInstance) => void;
}

export function PolicyTab({
  tc,
  installed,
  policyDrafts,
  busyKey,
  onPolicyDraftChange,
  onSavePolicy,
}: PolicyTabProps) {
  const { t } = useI18n();
  return (
    <div style={{ display: "grid", gap: 10 }}>
      {installed.length === 0 && (
        <div style={{ color: tc.textMuted, fontSize: 12 }}>{t("settings.nessunPluginInstallato")}</div>
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
                  onPolicyDraftChange(plugin.id, {
                    ...draft,
                    mode: event.target.value as PluginToolPolicy["mode"],
                  })
                }
                style={selectStyle(tc)}
              >
                <option value="all">{t("settings.all")}</option>
                <option value="allowlist">{t("settings.allowlist")}</option>
                <option value="denylist">{t("settings.denylist")}</option>
              </select>
              <input
                value={draft.tools}
                disabled={!plugin.canManage}
                onChange={(event) => onPolicyDraftChange(plugin.id, { ...draft, tools: event.target.value })}
                placeholder={t("settings.toolConsentitiCsv")}
                style={inputStyle(tc)}
              />
              <input
                value={draft.blockedTools}
                disabled={!plugin.canManage}
                onChange={(event) =>
                  onPolicyDraftChange(plugin.id, { ...draft, blockedTools: event.target.value })
                }
                placeholder={t("settings.toolBloccatiCsv")}
                style={inputStyle(tc)}
              />
              <div>
                <button
                  type="button"
                  disabled={!plugin.canManage || busyKey === `policy:${plugin.id}`}
                  onClick={() => onSavePolicy(plugin)}
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
  );
}
