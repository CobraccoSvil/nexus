"use client";

import { useCallback, useEffect, useState } from "react";
import { getHealth, getNeuralHealth, getProviderHealth } from "../lib/api-client";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";
import { PanelCard } from "./panel-card";
import {
  useProjectStore,
  selectProviderHealthChangedAt,
} from "../lib/project-dispatcher/store";

interface ServiceStatus {
  name: string;
  status: "ok" | "error" | "warning" | "loading";
  detail?: string;
  tipKey: string;
}

export function MonitoringPanel() {
  const tc = useThemeColors();
  const { t } = useI18n();

  const [services, setServices] = useState<ServiceStatus[]>([
    { name: "MCP Core", status: "loading", tipKey: "tip.mcpCore" },
    { name: "Neural Core", status: "loading", tipKey: "tip.neuralCore" },
    { name: "Database", status: "loading", tipKey: "tip.database" },
    { name: "Redis", status: "loading", tipKey: "tip.redis" },
  ]);

  const [providers, setProviders] = useState<ServiceStatus[]>([
    { name: "OpenAI", status: "loading", tipKey: "tip.openai" },
    { name: "Anthropic", status: "loading", tipKey: "tip.anthropic" },
    { name: "Google", status: "loading", tipKey: "tip.google" },
  ]);

  const refresh = useCallback(async () => {
    try {
      const h = await getHealth();
      setServices([
        { name: "MCP Core", status: "ok", tipKey: "tip.mcpCore" },
        { name: "Neural Core", status: h.components.neural_core ? "ok" : "error", tipKey: "tip.neuralCore" },
        { name: "Database", status: h.components.database ? "ok" : "error", tipKey: "tip.database" },
        { name: "Redis", status: h.components.redis ? "ok" : "error", tipKey: "tip.redis" },
      ]);
    } catch {
      try {
        const nh = await getNeuralHealth();
        setServices([
          { name: "MCP Core", status: "error", detail: "unreachable", tipKey: "tip.mcpCore" },
          { name: "Neural Core", status: nh.status === "ok" ? "ok" : "error", tipKey: "tip.neuralCore" },
          { name: "Database", status: "error", tipKey: "tip.database" },
          { name: "Redis", status: "error", tipKey: "tip.redis" },
        ]);
      } catch {
        setServices((prev) => prev.map((s) => ({ ...s, status: "error" as const, detail: "unreachable" })));
      }
    }

    for (const name of ["openai", "anthropic", "google"]) {
      try {
        const ph = await getProviderHealth(name);
        const reason = (ph.reason as string) || "";
        const isQuota = reason.includes("quota") || reason.includes("429") || reason.includes("rate");
        let status: "ok" | "warning" | "error";
        let detail = (ph.status as string) || "";
        if (ph.status === "ready") {
          status = "ok";
        } else if (isQuota) {
          status = "warning";
          detail = t("mon.quota");
        } else {
          status = "error";
        }
        setProviders((prev) =>
          prev.map((p) =>
            p.name.toLowerCase() === name ? { ...p, status, detail } : p,
          ),
        );
      } catch {
        setProviders((prev) =>
          prev.map((p) => (p.name.toLowerCase() === name ? { ...p, status: "error" as const, detail: t("mon.unreachable") } : p)),
        );
      }
    }
  }, [t]);

  // Fetch iniziale + fallback polling rilassato (120s)
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 120_000);
    return () => clearInterval(interval);
  }, [refresh]);

  // Event-driven: refresh immediato quando ProviderHealthChanged arriva via SSE
  const providerHealthAt = useProjectStore(selectProviderHealthChangedAt);
  useEffect(() => {
    if (providerHealthAt > 0) void refresh();
  }, [providerHealthAt, refresh]);

  const statusLabel = (status: "ok" | "error" | "warning" | "loading") =>
    status === "ok" ? t("mon.online") : status === "warning" ? t("mon.quota") : status === "error" ? t("mon.offline") : t("mon.checking");

  const dot = (status: "ok" | "error" | "warning" | "loading") => {
    const colors = { ok: tc.success, error: tc.error, warning: "#f59e0b", loading: tc.warning };
    return (
      <span
        style={{
          display: "inline-block",
          width: 8,
          height: 8,
          borderRadius: "50%",
          background: colors[status],
          marginRight: 8,
        }}
      />
    );
  };

  return (
    <PanelCard title={t("mon.title")} subtitle={t("mon.subtitle")}>
      <div style={{ fontSize: 13, lineHeight: 2 }}>
        <div style={{ fontWeight: 600, marginBottom: 4, color: tc.textSecondary }}>{t("mon.services")}</div>
        {services.map((s) => (
          <div key={s.name} title={`${t(s.tipKey as Parameters<typeof t>[0])}\nStatus: ${statusLabel(s.status)}`} style={{ cursor: "default" }}>
            {dot(s.status)} {s.name}
          </div>
        ))}
        <div style={{ fontWeight: 600, marginTop: 8, marginBottom: 4, color: tc.textSecondary }}>{t("mon.providers")}</div>
        {providers.map((p) => (
          <div key={p.name} title={`${t(p.tipKey as Parameters<typeof t>[0])}\nStatus: ${statusLabel(p.status)}${p.detail ? ` (${p.detail})` : ""}`} style={{ cursor: "default" }}>
            {dot(p.status)} {p.name} {p.detail && <span style={{ color: tc.textMuted, fontSize: 11 }}>({p.detail})</span>}
          </div>
        ))}
      </div>
    </PanelCard>
  );
}
