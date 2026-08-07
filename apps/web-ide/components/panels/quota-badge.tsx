"use client";

import { useState, useEffect, useCallback } from "react";
import { useThemeColors } from "../../lib/theme";
import { useProjectStore, selectOperationalRefreshAt } from "../../lib/project-dispatcher";
import { useI18n } from "../../lib/i18n";

interface QuotaBadgeProps {
  projectId: string;
}

interface QuotaData {
  quota: {
    max_ports: number;
    max_containers: number;
  };
  usage: {
    ports: number;
    containers: number;
  };
  audit_stats: {
    blocked_24h: number;
  };
}

/**
 * Badge compatto per l'header: mostra porte/container in uso vs quota.
 * Aggiornamento via SSE operativo (PortAllocated/Released, job, ...).
 */
export function QuotaBadge({ projectId }: QuotaBadgeProps) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const [data, setData] = useState<QuotaData | null>(null);
  const operationalRefreshAt = useProjectStore(selectOperationalRefreshAt);

  const fetchData = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/projects/${projectId}/security/quota`,
        { credentials: "include" }
      );
      if (res.ok) {
        setData(await res.json());
      }
    } catch { /* ignora */ }
  }, [projectId]);

  useEffect(() => {
    void fetchData();
  }, [fetchData, operationalRefreshAt]);

  if (!data) return null;

  const portsPct = data.quota.max_ports > 0
    ? (data.usage.ports / data.quota.max_ports) * 100
    : 0;
  const containersPct = data.quota.max_containers > 0
    ? (data.usage.containers / data.quota.max_containers) * 100
    : 0;

  const badgeColor = (pct: number) =>
    pct >= 90 ? "#ef4444" : pct >= 70 ? "#f59e0b" : tc.textMuted;

  return (
    <div style={{
      display: "flex", alignItems: "center", gap: 8,
      fontSize: 10, fontFamily: "var(--font-mono)",
      padding: "2px 8px", borderRadius: 4,
      background: tc.bgCard,
      border: `1px solid ${tc.border}`,
    }}>
      <span style={{ color: badgeColor(portsPct) }} title={t("panels.porteAllocateQuotaMax")}>
        P:{data.usage.ports}/{data.quota.max_ports}
      </span>
      <span style={{ color: tc.border }}>|</span>
      <span style={{ color: badgeColor(containersPct) }} title={t("panels.containerAttiviQuotaMax")}>
        C:{data.usage.containers}/{data.quota.max_containers}
      </span>
      {data.audit_stats.blocked_24h > 0 && (
        <>
          <span style={{ color: tc.border }}>|</span>
          <span style={{ color: "#ef4444", fontWeight: 600 }} title={t("panels.azioniBloccateNelleUltime")}>
            {data.audit_stats.blocked_24h} blk
          </span>
        </>
      )}
    </div>
  );
}
