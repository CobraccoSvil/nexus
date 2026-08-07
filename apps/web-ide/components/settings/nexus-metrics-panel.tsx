"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useHealthSnapshot } from "../../lib/hooks/use-health-snapshot";
import { useI18n } from "../../lib/i18n";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

interface PersistedTotals {
  pairs: number;
  visits: number;
  successes: number;
  failures: number;
  avg_q_value: number;
}

interface RouterStats {
  total_decisions: number;
  exploration_count: number;
  exploitation_count: number;
  cold_start_count: number;
  avg_decision_time_us: number;
  total_rewards: number;
  current_epsilon: number;
  // Stato APPRESO persistente (DB nexus_q_values): non azzerato dai restart.
  persisted?: PersistedTotals | null;
}

interface SchedulerStats {
  workers_registered: number;
  total_runs: number;
  total_failures: number;
}

interface NexusStats {
  status: string;
  router: RouterStats;
  scheduler: SchedulerStats;
  observability_ns: {
    name: string;
    entries: number;
  };
}

function StatCard({
  label,
  value,
  sub,
  color,
  tc,
}: {
  label: string;
  value: string | number;
  sub?: string;
  color?: string;
  tc: ReturnType<typeof useThemeColors>;
}) {
  return (
    <div
      style={{
        padding: "10px 14px",
        borderRadius: 8,
        background: tc.bgInput,
        border: `1px solid ${tc.border}`,
        minWidth: 110,
      }}
    >
      <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4, fontWeight: 500 }}>{label}</div>
      <div style={{ fontSize: 18, fontWeight: 700, color: color ?? tc.text, fontVariantNumeric: "tabular-nums" }}>
        {value}
      </div>
      {sub && <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>{sub}</div>}
    </div>
  );
}

function pct(num: number, denom: number): string {
  if (denom === 0) return "—";
  return `${((num / denom) * 100).toFixed(1)}%`;
}

function formatUs(us: number): string {
  if (us === 0) return "—";
  if (us < 1000) return `${us.toFixed(0)} µs`;
  return `${(us / 1000).toFixed(2)} ms`;
}

export function NexusMetricsPanel() {
  const { t } = useI18n();
  const tc = useThemeColors();
  const [stats, setStats] = useState<NexusStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const fetchStats = useCallback(async () => {
    try {
      const res = await fetch(`${API_BASE}/nexus/stats`, { credentials: "include" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data: NexusStats = await res.json();
      setStats(data);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento stats");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchStats();
  }, [fetchStats]);

  const onHealthSnapshot = useCallback(() => {
    void fetchStats();
  }, [fetchStats]);
  useHealthSnapshot(onHealthSnapshot);

  const r = stats?.router;
  const s = stats?.scheduler;

  const explorationPct = r ? pct(r.exploration_count, r.total_decisions) : "—";
  const exploitationPct = r ? pct(r.exploitation_count, r.total_decisions) : "—";
  const coldStartPct = r ? pct(r.cold_start_count, r.total_decisions) : "—";

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
        <span style={{ fontSize: 14, fontWeight: 700 }}>📊 Nexus Router — Metriche Live</span>
        {loading && (
          <span className="text-xs text-muted">caricamento…</span>
        )}
        {!loading && !error && (
          <span style={{
            fontSize: 10,
            fontWeight: 600,
            background: "#22c55e20",
            color: "#22c55e",
            border: "1px solid #22c55e40",
            borderRadius: 6,
            padding: "1px 7px",
          }}>
            {t("badge.live")}
          </span>
        )}
        {!loading && error && (
          <span style={{
            fontSize: 10,
            fontWeight: 600,
            background: "#ef444420",
            color: "#ef4444",
            border: "1px solid #ef444440",
            borderRadius: 6,
            padding: "1px 7px",
          }}>
            {t("badge.offline")}
          </span>
        )}
      </div>
      <div style={{ color: tc.textMuted, fontSize: 12, marginBottom: 14 }}>
        Snapshot in tempo reale del Q-Learning router. Aggiornato via health-monitor condiviso (~5s).
      </div>

      {error ? (
        <div style={{ color: tc.textMuted, fontSize: 12, padding: "10px 14px", borderRadius: 8, background: tc.bgHover }}>
          {error} — il backend potrebbe non essere raggiungibile.
        </div>
      ) : (
        <>
          {/* Stato APPRESO persistente (DB): sopravvive ai restart. Distingue
              la "conoscenza" accumulata dalla mera attivita' di sessione, cosi'
              un riavvio del backend non fa sembrare il router 'spento'. */}
          {(() => {
            const p = r?.persisted;
            const successRate =
              p && p.successes + p.failures > 0
                ? pct(p.successes, p.successes + p.failures)
                : "—";
            return (
              <>
                <div style={{ fontSize: 12, fontWeight: 700, color: tc.textSecondary, marginBottom: 2 }}>
                  {t("settings.statoAppresoPersistente")}
                </div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8 }}>
                  Conoscenza Q-Learning accumulata nel DB — non si azzera ai riavvii del backend.
                </div>
                <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 16 }}>
                  <StatCard
                    label="Coppie task/agent"
                    value={p ? p.pairs : "—"}
                    sub="apprese"
                    color="#a855f7"
                    tc={tc}
                  />
                  <StatCard
                    label="Visite totali"
                    value={p ? p.visits : "—"}
                    sub="cumulate (tutte le sessioni)"
                    color="#a855f7"
                    tc={tc}
                  />
                  <StatCard
                    label="Tasso successo"
                    value={successRate}
                    sub={p ? `${p.successes} ok / ${p.failures} ko` : "—"}
                    color="#22c55e"
                    tc={tc}
                  />
                  <StatCard
                    label="Q-value medio"
                    value={p ? p.avg_q_value.toFixed(3) : "—"}
                    sub="media appresa"
                    color="#06b6d4"
                    tc={tc}
                  />
                </div>
              </>
            );
          })()}

          {/* Router Q-Learning stats */}
          <div style={{ fontSize: 12, fontWeight: 700, color: tc.textSecondary, marginBottom: 2 }}>{t("settings.routerQLearningSessione")}</div>
          <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8 }}>
            Contatori da quando il backend e&apos; stato avviato (azzerati a ogni restart).
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 16 }}>
            <StatCard
              label="Decisioni totali"
              value={r?.total_decisions ?? "—"}
              tc={tc}
            />
            <StatCard
              label="ε (epsilon)"
              value={r ? r.current_epsilon.toFixed(3) : "—"}
              sub="esplorazione corrente"
              color="#6366f1"
              tc={tc}
            />
            <StatCard
              label="Esplorazione"
              value={explorationPct}
              sub={r ? `${r.exploration_count} routing` : "—"}
              color="#f59e0b"
              tc={tc}
            />
            <StatCard
              label="Exploitation"
              value={exploitationPct}
              sub={r ? `${r.exploitation_count} routing` : "—"}
              color="#22c55e"
              tc={tc}
            />
            <StatCard
              label="Cold start"
              value={coldStartPct}
              sub={r ? `${r.cold_start_count} routing` : "—"}
              color={tc.textMuted}
              tc={tc}
            />
            <StatCard
              label="Latenza media"
              value={r ? formatUs(r.avg_decision_time_us) : "—"}
              sub="decision time"
              tc={tc}
            />
            <StatCard
              label="Total rewards"
              value={r ? r.total_rewards.toFixed(2) : "—"}
              sub="cumulated RL reward"
              color="#06b6d4"
              tc={tc}
            />
          </div>

          {/* Barre esplorazione vs exploitation */}
          {r && r.total_decisions > 0 && (
            <div style={{ marginBottom: 16 }}>
              <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 6, fontWeight: 500 }}>
                {t("settings.distribuzioneRouting")}
              </div>
              <div style={{ display: "flex", height: 10, borderRadius: 6, overflow: "hidden", background: tc.bgHover }}>
                <div
                  style={{
                    width: `${(r.exploration_count / r.total_decisions) * 100}%`,
                    background: "#f59e0b",
                    transition: "width 0.4s",
                  }}
                  title={`Exploration: ${explorationPct}`}
                />
                <div
                  style={{
                    width: `${(r.exploitation_count / r.total_decisions) * 100}%`,
                    background: "#22c55e",
                    transition: "width 0.4s",
                  }}
                  title={`Exploitation: ${exploitationPct}`}
                />
                <div
                  style={{
                    flex: 1,
                    background: tc.border,
                  }}
                  title={`Cold start: ${coldStartPct}`}
                />
              </div>
              <div style={{ display: "flex", gap: 14, marginTop: 5 }}>
                {[
                  { color: "#f59e0b", label: `Exploration ${explorationPct}` },
                  { color: "#22c55e", label: `Exploitation ${exploitationPct}` },
                  { color: tc.border, label: `Cold start ${coldStartPct}` },
                ].map((item) => (
                  <div key={item.label} style={{ display: "flex", alignItems: "center", gap: 5 }}>
                    <div style={{ width: 8, height: 8, borderRadius: "50%", background: item.color, flexShrink: 0 }} />
                    <span style={{ fontSize: 10, color: tc.textMuted }}>{item.label}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Scheduler summary */}
          <div style={{ fontSize: 12, fontWeight: 700, color: tc.textSecondary, marginBottom: 8 }}>{t("settings.learningWorkers")}</div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <StatCard
              label="Workers attivi"
              value={s?.workers_registered ?? "—"}
              sub="registrati"
              color="#6366f1"
              tc={tc}
            />
            <StatCard
              label="Runs totali"
              value={s?.total_runs ?? "—"}
              tc={tc}
            />
            <StatCard
              label="Failures"
              value={s?.total_failures ?? "—"}
              color={s && s.total_failures > 0 ? "#ef4444" : tc.textMuted}
              tc={tc}
            />
            <StatCard
              label="Namespace entries"
              value={stats?.observability_ns.entries ?? "—"}
              sub={stats?.observability_ns.name}
              tc={tc}
            />
          </div>
        </>
      )}
    </div>
  );
}
