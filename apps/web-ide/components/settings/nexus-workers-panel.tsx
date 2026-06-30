"use client";

import { useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";
const POLL_INTERVAL_MS = 10000;

interface WorkerStat {
  runs: number;
  failures: number;
  total_duration_ms: number;
}

interface NexusStatsSlim {
  status: string;
  scheduler: {
    workers_registered: number;
    total_runs: number;
    total_failures: number;
    per_worker: Record<string, WorkerStat>;
  };
}

// Worker metadata (trigger type for display) — keys match worker.name() in Rust
const WORKER_META: Record<string, { trigger: string; emoji: string; label: string }> = {
  ultralearn:          { trigger: "OnTaskComplete", emoji: "🧠", label: "Ultralearn" },
  audit:               { trigger: "OnTaskComplete", emoji: "🔒", label: "Audit" },
  metrics_aggregation: { trigger: "OnTaskComplete", emoji: "📈", label: "Metrics Aggregation" },
  versioning:          { trigger: "OnTaskComplete", emoji: "📌", label: "Versioning" },
  profiling:           { trigger: "Periodic",       emoji: "⏱️",  label: "Profiling" },
  anomaly_detection:   { trigger: "Periodic",       emoji: "🚨", label: "Anomaly Detection" },
  memory_consolidation:{ trigger: "Periodic",       emoji: "🗜️",  label: "Memory Consolidation" },
  cleanup:             { trigger: "Periodic",       emoji: "🧹", label: "Cleanup" },
  session_persistence: { trigger: "Periodic",       emoji: "💾", label: "Session Persistence" },
  q_learning_replay:   { trigger: "Periodic",       emoji: "♻️",  label: "Q-Learning Replay" },
  replication:         { trigger: "Periodic",       emoji: "🔄", label: "Replication" },
  clustering:          { trigger: "Periodic",       emoji: "🔵", label: "Clustering" },
};

function avgDuration(ws: WorkerStat): string {
  if (ws.runs === 0) return "—";
  const avg = ws.total_duration_ms / ws.runs;
  if (avg < 1) return "<1 ms";
  return `${avg.toFixed(0)} ms`;
}

function status(ws: WorkerStat): { label: string; color: string; bg: string } {
  if (ws.runs === 0) return { label: "idle",     color: "#94a3b8", bg: "#94a3b820" };
  const failRate = ws.failures / ws.runs;
  if (failRate > 0.5) return { label: "degraded", color: "#ef4444", bg: "#ef444420" };
  if (failRate > 0)   return { label: "warning",  color: "#f59e0b", bg: "#f59e0b20" };
  return               { label: "ok",             color: "#22c55e", bg: "#22c55e20" };
}

export function NexusWorkersPanel() {
  const tc = useThemeColors();
  const [stats, setStats] = useState<NexusStatsSlim | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchStats = async () => {
    try {
      const res = await fetch(`${API_BASE}/nexus/stats`, { credentials: "include" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data: NexusStatsSlim = await res.json();
      setStats(data);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchStats();
    intervalRef.current = setInterval(fetchStats, POLL_INTERVAL_MS);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, []);

  const perWorker = stats?.scheduler.per_worker ?? {};

  // Build rows: known workers first, then any unknown ones
  const knownNames = Object.keys(WORKER_META);
  const unknownNames = Object.keys(perWorker).filter((n) => !knownNames.includes(n));
  const allNames = [...knownNames, ...unknownNames];

  return (
    <div
      style={{
        padding: 18,
        borderRadius: 12,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
        <span style={{ fontSize: 14, fontWeight: 700 }}>⚙️ Learning Workers — Status</span>
        {loading && <span className="text-xs text-muted">caricamento…</span>}
        {!loading && !error && stats && (
          <span style={{
            fontSize: 10, fontWeight: 700,
            background: "#6366f120", color: "#6366f1", border: "1px solid #6366f140",
            borderRadius: 6, padding: "1px 7px",
          }}>
            {stats.scheduler.workers_registered} workers
          </span>
        )}
      </div>
      <div style={{ color: tc.textMuted, fontSize: 12, marginBottom: 14 }}>
        12 background workers attivi. I worker <em>OnTaskComplete</em> scattano dopo ogni esecuzione agente;
        i worker <em>Periodic</em> girano ogni 60s.
      </div>

      {error ? (
        <div style={{ color: tc.textMuted, fontSize: 12, padding: "10px 14px", borderRadius: 8, background: tc.bgHover }}>
          {error} — backend non raggiungibile
        </div>
      ) : (
        <div style={{ overflowX: "auto" }}>
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}>
            <thead>
              <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
                {["Worker", "Trigger", "Runs", "Failures", "Avg duration", "Status"].map((h) => (
                  <th
                    key={h}
                    style={{
                      textAlign: "left",
                      padding: "6px 10px",
                      fontWeight: 600,
                      color: tc.textMuted,
                      whiteSpace: "nowrap",
                    }}
                  >
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {allNames.map((name) => {
                const ws: WorkerStat = perWorker[name] ?? { runs: 0, failures: 0, total_duration_ms: 0 };
                const meta = WORKER_META[name] ?? { trigger: "Unknown", emoji: "❓", label: name };
                const st = status(ws);
                const displayName = meta.label;

                return (
                  <tr
                    key={name}
                    style={{ borderBottom: `1px solid ${tc.border}20` }}
                  >
                    <td style={{ padding: "7px 10px", whiteSpace: "nowrap", fontWeight: 500 }}>
                      <span style={{ marginRight: 6 }}>{meta.emoji}</span>
                      {displayName}
                    </td>
                    <td style={{ padding: "7px 10px", color: tc.textMuted, whiteSpace: "nowrap" }}>
                      <span style={{
                        fontSize: 10, fontWeight: 600,
                        background: meta.trigger === "Periodic" ? "#6366f115" : "#06b6d415",
                        color: meta.trigger === "Periodic" ? "#6366f1" : "#06b6d4",
                        border: `1px solid ${meta.trigger === "Periodic" ? "#6366f130" : "#06b6d430"}`,
                        borderRadius: 5, padding: "1px 6px",
                      }}>
                        {meta.trigger}
                      </span>
                    </td>
                    <td style={{ padding: "7px 10px", fontVariantNumeric: "tabular-nums" }}>
                      {ws.runs}
                    </td>
                    <td style={{
                      padding: "7px 10px",
                      fontVariantNumeric: "tabular-nums",
                      color: ws.failures > 0 ? "#ef4444" : tc.textMuted,
                    }}>
                      {ws.failures}
                    </td>
                    <td style={{ padding: "7px 10px", color: tc.textMuted, fontVariantNumeric: "tabular-nums" }}>
                      {avgDuration(ws)}
                    </td>
                    <td style={{ padding: "7px 10px" }}>
                      <span style={{
                        fontSize: 10, fontWeight: 700,
                        background: st.bg, color: st.color,
                        border: `1px solid ${st.color}40`,
                        borderRadius: 5, padding: "2px 8px",
                      }}>
                        {st.label}
                      </span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
