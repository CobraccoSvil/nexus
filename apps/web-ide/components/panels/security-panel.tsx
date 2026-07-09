"use client";

import { useState, useEffect, useCallback } from "react";
import { useThemeColors } from "../../lib/theme";
import { useProjectStore, selectOperationalRefreshAt } from "../../lib/project-dispatcher";

interface SecurityPanelProps {
  projectId: string;
  onSendToChat?: (message: string) => void;
}

interface AuditItem {
  id: number;
  ts: string;
  actor: string;
  action: string;
  resource_kind: string;
  resource_id: string | null;
  outcome: string;
  details: Record<string, unknown>;
}

interface QuotaInfo {
  quota: {
    max_ports: number;
    max_memory_mb: number;
    max_disk_mb: number;
    max_containers: number;
    max_db_pool_size: number;
  };
  usage: {
    ports: number;
    containers: number;
  };
  audit_stats: {
    events_24h: number;
    blocked_24h: number;
  };
}

const OUTCOME_COLORS: Record<string, string> = {
  allowed: "#10b981",
  blocked: "#ef4444",
  killed: "#f59e0b",
};

const OUTCOME_LABELS: Record<string, string> = {
  allowed: "OK",
  blocked: "Bloccato",
  killed: "Terminato",
};

export function SecurityPanel({ projectId }: SecurityPanelProps) {
  const tc = useThemeColors();
  const [items, setItems] = useState<AuditItem[]>([]);
  const [quota, setQuota] = useState<QuotaInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState<string>("all");
  const [total, setTotal] = useState(0);
  const operationalRefreshAt = useProjectStore(selectOperationalRefreshAt);

  const fetchAudit = useCallback(async () => {
    setLoading(true);
    try {
      const params = new URLSearchParams({ limit: "50" });
      if (filter !== "all") params.set("outcome", filter);
      const res = await fetch(
        `/api/projects/${projectId}/security/audit?${params}`,
        { credentials: "include" }
      );
      if (res.ok) {
        const data = await res.json();
        setItems(data.items ?? []);
        setTotal(data.total ?? 0);
      }
    } catch { /* ignora */ } finally {
      setLoading(false);
    }
  }, [projectId, filter]);

  const fetchQuota = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/projects/${projectId}/security/quota`,
        { credentials: "include" }
      );
      if (res.ok) {
        setQuota(await res.json());
      }
    } catch { /* ignora */ }
  }, [projectId]);

  useEffect(() => {
    void fetchAudit();
    void fetchQuota();
  }, [fetchAudit, fetchQuota, operationalRefreshAt]);

  const formatTime = (ts: string) => {
    try {
      const d = new Date(ts);
      return d.toLocaleTimeString("it-IT", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    } catch {
      return ts;
    }
  };

  const usageBar = (used: number, max: number, label: string) => {
    const pct = max > 0 ? Math.min((used / max) * 100, 100) : 0;
    const color = pct >= 90 ? "#ef4444" : pct >= 70 ? "#f59e0b" : "#10b981";
    return (
      <div style={{ flex: 1, minWidth: 120 }}>
        <div style={{ fontSize: 10, color: tc.textMuted, marginBottom: 2 }}>{label}</div>
        <div style={{
          display: "flex", alignItems: "center", gap: 6,
          background: tc.bgCard, borderRadius: 4, padding: "2px 6px",
          border: `1px solid ${tc.border}`,
        }}>
          <div style={{
            height: 6, flex: 1, borderRadius: 3,
            background: `${tc.border}`,
            position: "relative", overflow: "hidden",
          }}>
            <div style={{
              position: "absolute", top: 0, left: 0, height: "100%",
              width: `${pct}%`, background: color, borderRadius: 3,
              transition: "width 0.3s ease",
            }} />
          </div>
          <span style={{ fontSize: 10, fontFamily: "var(--font-mono)", color: tc.text, whiteSpace: "nowrap" }}>
            {used}/{max}
          </span>
        </div>
      </div>
    );
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      {/* Barra superiore con quote */}
      <div style={{
        display: "flex", alignItems: "center", gap: 12,
        padding: "6px 12px", borderBottom: `1px solid ${tc.border}`,
        flexShrink: 0, flexWrap: "wrap",
      }}>
        {quota && (
          <>
            {usageBar(quota.usage.ports, quota.quota.max_ports, "Porte")}
            {usageBar(quota.usage.containers, quota.quota.max_containers, "Container")}
            <div style={{ fontSize: 10, color: tc.textMuted, display: "flex", gap: 8 }}>
              <span>24h: {quota.audit_stats.events_24h} eventi</span>
              {quota.audit_stats.blocked_24h > 0 && (
                <span style={{ color: "#ef4444", fontWeight: 600 }}>
                  {quota.audit_stats.blocked_24h} bloccati
                </span>
              )}
            </div>
          </>
        )}
        {/* Filtro outcome */}
        <select
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          style={{
            fontSize: 11, padding: "2px 6px", borderRadius: 4,
            border: `1px solid ${tc.border}`, background: tc.bgCard,
            color: tc.text, cursor: "pointer",
          }}
        >
          <option value="all">Tutti</option>
          <option value="allowed">OK</option>
          <option value="blocked">Bloccati</option>
          <option value="killed">Terminati</option>
        </select>
        <button
          onClick={() => { fetchAudit(); fetchQuota(); }}
          style={{
            fontSize: 10, padding: "2px 8px", borderRadius: 4,
            border: `1px solid ${tc.border}`, background: tc.bgCard,
            color: tc.textMuted, cursor: "pointer",
          }}
        >
          {loading ? "..." : "Aggiorna"}
        </button>
      </div>

      {/* Lista audit */}
      <div style={{ padding: 8, overflow: "auto", flex: 1, minHeight: 0 }}>
        {items.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 12, padding: 12 }}>
            {loading ? "Caricamento..." : "Nessun evento di audit registrato."}
          </div>
        ) : (
          <table style={{ width: "100%", fontSize: 11, borderCollapse: "collapse" }}>
            <thead>
              <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
                <th style={thStyle(tc)}>Ora</th>
                <th style={thStyle(tc)}>Azione</th>
                <th style={thStyle(tc)}>Risorsa</th>
                <th style={thStyle(tc)}>Esito</th>
                <th style={thStyle(tc)}>Dettagli</th>
              </tr>
            </thead>
            <tbody>
              {items.map((item) => (
                <tr key={item.id} style={{
                  borderBottom: `1px solid ${tc.border}`,
                  background: item.outcome === "blocked" ? "rgba(239,68,68,0.05)" :
                    item.outcome === "killed" ? "rgba(245,158,11,0.05)" : "transparent",
                }}>
                  <td style={tdStyle(tc)}>{formatTime(item.ts)}</td>
                  <td style={tdStyle(tc)}>
                    <span style={{ fontFamily: "var(--font-mono)" }}>{item.action}</span>
                  </td>
                  <td style={tdStyle(tc)}>
                    <span style={{
                      background: tc.accentBg, borderRadius: 3,
                      padding: "0 4px", fontSize: 10, marginRight: 4,
                    }}>
                      {item.resource_kind}
                    </span>
                    {item.resource_id && (
                      <span style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>
                        {item.resource_id}
                      </span>
                    )}
                  </td>
                  <td style={tdStyle(tc)}>
                    <span style={{
                      color: OUTCOME_COLORS[item.outcome] ?? tc.text,
                      fontWeight: 600, fontSize: 10,
                    }}>
                      {OUTCOME_LABELS[item.outcome] ?? item.outcome}
                    </span>
                  </td>
                  <td style={{ ...tdStyle(tc), maxWidth: 200, overflow: "hidden", textOverflow: "ellipsis" }}>
                    <span style={{ fontSize: 10, color: tc.textMuted, fontFamily: "var(--font-mono)" }}>
                      {Object.keys(item.details).length > 0
                        ? JSON.stringify(item.details).slice(0, 80)
                        : "-"}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {total > items.length && (
          <div style={{ color: tc.textMuted, fontSize: 10, padding: "8px 0", textAlign: "center" }}>
            Mostrati {items.length} di {total} eventi
          </div>
        )}
      </div>
    </div>
  );
}

function thStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    textAlign: "left" as const,
    padding: "4px 8px",
    color: tc.textMuted,
    fontWeight: 500,
    fontSize: 10,
    whiteSpace: "nowrap" as const,
  };
}

function tdStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    padding: "4px 8px",
    color: tc.text,
    whiteSpace: "nowrap" as const,
  };
}
