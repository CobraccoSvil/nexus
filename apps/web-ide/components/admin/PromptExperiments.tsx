"use client";

/**
 * PromptExperiments — lista e gestione esperimenti A/B canary (Fase 3).
 *
 * Mostra gli esperimenti running con possibilita' di:
 * - Forzare la promozione manuale
 * - Forzare lo scarto
 * - Espandere il diff baseline vs variante
 */

import { useEffect, useState, useCallback } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import {
  listPromptExperiments,
  getPromptExperiment,
  forcePromoteExperiment,
  forceDiscardExperiment,
  type PromptExperiment,
} from "../../lib/api-client";

const STATUS_BG: Record<string, string> = {
  running: "#dbeafe",
  promoted: "#dcfce7",
  discarded: "#f3f4f6",
  rolled_back: "#fee2e2",
};
const STATUS_FG: Record<string, string> = {
  running: "#1e40af",
  promoted: "#166534",
  discarded: "#6b7280",
  rolled_back: "#991b1b",
};
const STATUS_LABELS: Record<string, string> = {
  running: "In corso",
  promoted: "Promosso",
  discarded: "Scartato",
  rolled_back: "Rollback",
};

function DeltaBadge({ baseline, variant }: { baseline?: number; variant?: number }) {
  const tc = useThemeColors();
  if (baseline === undefined || variant === undefined) {
    return <span style={{ color: tc.textSecondary }}>--</span>;
  }
  const delta = variant - baseline;
  const color = delta > 0.02 ? "#16a34a" : delta < -0.02 ? "#dc2626" : tc.textSecondary;
  return (
    <span style={{ fontWeight: 500, color }}>
      {delta >= 0 ? "+" : ""}{Math.round(delta * 1000) / 10}pp
    </span>
  );
}

export default function PromptExperiments() {
  const tc = useThemeColors();
  // Dialog di Nexus (no window.confirm/alert nativi).
  const { confirmDialog, alertDialog } = useGlobalDialog();
  const [experiments, setExperiments] = useState<PromptExperiment[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<"all" | "running">("running");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [expandedData, setExpandedData] = useState<PromptExperiment | null>(null);
  const [expandLoading, setExpandLoading] = useState(false);
  const [actionLoading, setActionLoading] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const { experiments: exps } = await listPromptExperiments();
      setExperiments(exps);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    const interval = setInterval(load, 30000);
    return () => clearInterval(interval);
  }, [load]);

  const handleExpand = async (id: string) => {
    if (expanded === id) {
      setExpanded(null);
      setExpandedData(null);
      return;
    }
    setExpanded(id);
    setExpandLoading(true);
    try {
      const detail = await getPromptExperiment(id);
      setExpandedData(detail);
    } catch {
      setExpandedData(null);
    } finally {
      setExpandLoading(false);
    }
  };

  const handleAction = async (id: string, action: "promote" | "discard") => {
    const label = action === "promote" ? "promuovere" : "scartare";
    const ok = await confirmDialog({
      title: action === "promote" ? "Promuovi esperimento" : "Scarta esperimento",
      message: `Confermi di voler ${label} questo esperimento?`,
      danger: action === "discard",
      confirmLabel: action === "promote" ? "Promuovi" : "Scarta",
      cancelLabel: "Annulla",
    });
    if (!ok) return;
    setActionLoading(id + action);
    try {
      if (action === "promote") {
        await forcePromoteExperiment(id);
      } else {
        await forceDiscardExperiment(id);
      }
      await load();
      if (expanded === id) {
        setExpanded(null);
        setExpandedData(null);
      }
    } catch (e) {
      await alertDialog(
        e instanceof Error ? e.message : String(e),
        action === "promote" ? "Promozione fallita" : "Scarto fallito",
      );
    } finally {
      setActionLoading(null);
    }
  };

  const displayed = experiments.filter(
    (e) => filter === "all" || e.status === "running"
  );

  const runningCount = experiments.filter((e) => e.status === "running").length;

  const filterBtn = (active: boolean): React.CSSProperties => ({
    padding: "6px 14px",
    fontSize: 13,
    borderRadius: 6,
    border: `1px solid ${active ? tc.accent : tc.border}`,
    background: active ? tc.accent : tc.bgCard,
    color: active ? "#fff" : tc.text,
    cursor: "pointer",
    fontWeight: active ? 600 : 400,
  });

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
      {/* Header */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <div style={{ display: "flex", gap: 8 }}>
          <button onClick={() => setFilter("running")} style={filterBtn(filter === "running")}>
            In corso
            {runningCount > 0 && (
              <span style={{ marginLeft: 8, background: "#dbeafe", color: "#1e40af", fontSize: 11, padding: "2px 6px", borderRadius: 10 }}>
                {runningCount}
              </span>
            )}
          </button>
          <button onClick={() => setFilter("all")} style={filterBtn(filter === "all")}>
            Tutti ({experiments.length})
          </button>
        </div>
        <button
          onClick={load}
          disabled={loading}
          style={{
            fontSize: 13, color: tc.accent, background: "transparent", border: "none",
            cursor: loading ? "not-allowed" : "pointer", opacity: loading ? 0.5 : 1, textDecoration: "underline",
          }}
        >
          {loading ? "Caricamento..." : "Aggiorna"}
        </button>
      </div>

      {error && (
        <div style={{ background: "#fef2f2", border: "1px solid #fecaca", color: "#b91c1c", fontSize: 13, borderRadius: 8, padding: "10px 14px" }}>
          {error}
        </div>
      )}

      {displayed.length === 0 && !loading ? (
        <div style={{ textAlign: "center", padding: "48px 0", color: tc.textSecondary }}>
          {filter === "running"
            ? "Nessun esperimento in corso. Il PromptOptimizerWorker ne avviera' di nuovi quando i prompt scendono sotto soglia."
            : "Nessun esperimento registrato."}
        </div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {displayed.map((exp) => (
            <ExperimentCard
              key={exp.id}
              experiment={exp}
              isExpanded={expanded === exp.id}
              expandedData={expanded === exp.id ? expandedData : null}
              expandLoading={expandLoading && expanded === exp.id}
              actionLoading={actionLoading}
              onExpand={() => handleExpand(exp.id)}
              onPromote={() => handleAction(exp.id, "promote")}
              onDiscard={() => handleAction(exp.id, "discard")}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ExperimentCard({
  experiment: exp,
  isExpanded,
  expandedData,
  expandLoading,
  actionLoading,
  onExpand,
  onPromote,
  onDiscard,
}: {
  experiment: PromptExperiment;
  isExpanded: boolean;
  expandedData: PromptExperiment | null;
  expandLoading: boolean;
  actionLoading: string | null;
  onExpand: () => void;
  onPromote: () => void;
  onDiscard: () => void;
}) {
  const tc = useThemeColors();
  const isRunning = exp.status === "running";
  const startedAt = new Date(exp.started_at).toLocaleDateString("it-IT", {
    day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit",
  });

  return (
    <div
      style={{
        background: tc.bgCard,
        border: `1px solid ${isRunning ? "#93c5fd" : tc.border}`,
        borderRadius: 8,
        overflow: "hidden",
      }}
    >
      {/* Riga principale */}
      <div style={{ display: "flex", alignItems: "center", padding: "12px 16px", gap: 16 }}>
        <button
          onClick={onExpand}
          style={{ flex: 1, textAlign: "left", background: "none", border: "none", cursor: "pointer", minWidth: 0, padding: 0, color: tc.text }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
            <span
              style={{
                fontSize: 11, padding: "2px 8px", borderRadius: 10, fontWeight: 500,
                background: STATUS_BG[exp.status] ?? "#f3f4f6",
                color: STATUS_FG[exp.status] ?? "#6b7280",
              }}
            >
              {STATUS_LABELS[exp.status] ?? exp.status}
            </span>
            <span style={{ fontFamily: "var(--font-mono)", fontSize: 13, color: tc.text }}>
              {exp.prompt_key}
            </span>
            <span style={{ fontSize: 12, color: tc.textSecondary }}>
              v{exp.baseline_version} → v{exp.variant_version}
            </span>
            {isRunning && (
              <span style={{ fontSize: 11, background: "#f3f4f6", padding: "2px 8px", borderRadius: 4, color: "#6b7280" }}>
                {exp.traffic_pct}% traffico variante
              </span>
            )}
          </div>
          <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 4 }}>Avviato {startedAt}</div>
        </button>

        {/* Metriche compatte */}
        <div style={{ display: "flex", alignItems: "center", gap: 24, fontSize: 13, flexShrink: 0 }}>
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 11, color: tc.textSecondary }}>Baseline</div>
            <div style={{ fontWeight: 500, color: tc.text }}>
              {exp.baseline_success_rate !== undefined && exp.baseline_success_rate !== null
                ? `${Math.round(exp.baseline_success_rate * 100)}%`
                : "--"}
            </div>
          </div>
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 11, color: tc.textSecondary }}>Variante</div>
            <div style={{ fontWeight: 500, color: tc.text }}>
              {exp.variant_success_rate !== undefined && exp.variant_success_rate !== null
                ? `${Math.round(exp.variant_success_rate * 100)}%`
                : "--"}
            </div>
          </div>
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: 11, color: tc.textSecondary }}>Delta</div>
            <DeltaBadge baseline={exp.baseline_success_rate} variant={exp.variant_success_rate} />
          </div>
        </div>

        {/* Azioni manuali */}
        {isRunning && (
          <div style={{ display: "flex", gap: 8, flexShrink: 0 }}>
            <button
              onClick={onPromote}
              disabled={!!actionLoading}
              style={{
                padding: "6px 12px", fontSize: 12, background: "#16a34a", color: "#fff",
                border: "none", borderRadius: 6, cursor: actionLoading ? "not-allowed" : "pointer",
                opacity: actionLoading ? 0.5 : 1,
              }}
            >
              {actionLoading === exp.id + "promote" ? "..." : "Promuovi"}
            </button>
            <button
              onClick={onDiscard}
              disabled={!!actionLoading}
              style={{
                padding: "6px 12px", fontSize: 12, background: "#e5e7eb", color: "#374151",
                border: "none", borderRadius: 6, cursor: actionLoading ? "not-allowed" : "pointer",
                opacity: actionLoading ? 0.5 : 1,
              }}
            >
              {actionLoading === exp.id + "discard" ? "..." : "Scarta"}
            </button>
          </div>
        )}
        {exp.decision_reason && !isRunning && (
          <div style={{ fontSize: 12, color: tc.textSecondary, maxWidth: 200, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={exp.decision_reason}>
            {exp.decision_reason}
          </div>
        )}
      </div>

      {/* Dettaglio espanso */}
      {isExpanded && (
        <div style={{ borderTop: `1px solid ${tc.border}`, background: tc.bgHeader, padding: 16 }}>
          {expandLoading ? (
            <div style={{ fontSize: 13, color: tc.textSecondary, textAlign: "center", padding: "16px 0" }}>Caricamento dettaglio...</div>
          ) : expandedData ? (
            <ExperimentDetail data={expandedData} />
          ) : (
            <div style={{ fontSize: 13, color: tc.textSecondary, textAlign: "center", padding: "16px 0" }}>Dati non disponibili</div>
          )}
        </div>
      )}
    </div>
  );
}

function ExperimentDetail({ data }: { data: PromptExperiment }) {
  const tc = useThemeColors();
  const [showDiff, setShowDiff] = useState(false);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {/* Stats reflection */}
      {(data.baseline_stats || data.variant_stats) && (
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
          {[
            { label: "Baseline", stats: data.baseline_stats, version: data.baseline_version },
            { label: "Variante", stats: data.variant_stats, version: data.variant_version },
          ].map(({ label, stats, version }) => (
            <div key={label} style={{ background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, padding: 12 }}>
              <div style={{ fontSize: 11, fontWeight: 500, color: tc.textSecondary, textTransform: "uppercase", marginBottom: 8 }}>
                {label} — v{version}
              </div>
              {stats ? (
                <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8, fontSize: 13 }}>
                  <div>
                    <div style={{ fontSize: 11, color: tc.textSecondary }}>Run reflection</div>
                    <div style={{ fontWeight: 500 }}>{stats.runs}</div>
                  </div>
                  <div>
                    <div style={{ fontSize: 11, color: tc.textSecondary }}>Score medio</div>
                    <div style={{ fontWeight: 500 }}>{Math.round(stats.avg_score * 100)}%</div>
                  </div>
                  <div>
                    <div style={{ fontSize: 11, color: tc.textSecondary }}>Min</div>
                    <div>{Math.round(stats.min_score * 100)}%</div>
                  </div>
                  <div>
                    <div style={{ fontSize: 11, color: tc.textSecondary }}>Max</div>
                    <div>{Math.round(stats.max_score * 100)}%</div>
                  </div>
                </div>
              ) : (
                <div style={{ fontSize: 12, color: tc.textSecondary }}>Nessun dato reflection</div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Diff prompt */}
      {(data.baseline_content || data.variant_content) && (
        <div>
          <button
            onClick={() => setShowDiff(!showDiff)}
            style={{ fontSize: 13, color: tc.accent, background: "none", border: "none", cursor: "pointer", textDecoration: "underline" }}
          >
            {showDiff ? "Nascondi" : "Mostra"} diff baseline / variante
          </button>
          {showDiff && (
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, marginTop: 12 }}>
              {[
                { label: "Baseline", content: data.baseline_content, borderColor: "#fecaca" },
                { label: "Variante", content: data.variant_content, borderColor: "#bbf7d0" },
              ].map(({ label, content, borderColor }) => (
                <div key={label}>
                  <div style={{ fontSize: 11, fontWeight: 500, color: tc.textSecondary, marginBottom: 4 }}>{label}</div>
                  <pre
                    style={{
                      fontSize: 11, background: tc.bgCard, border: `1px solid ${borderColor}`,
                      borderRadius: 6, padding: 12, overflow: "auto", maxHeight: 256,
                      whiteSpace: "pre-wrap", fontFamily: "var(--font-mono)", color: tc.text,
                    }}
                  >
                    {content ?? "(nessun contenuto)"}
                  </pre>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {data.decision_reason && (
        <div style={{ fontSize: 12, color: tc.textSecondary, background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 6, padding: 12 }}>
          <span style={{ fontWeight: 500 }}>Motivazione decisione:</span> {data.decision_reason}
        </div>
      )}
    </div>
  );
}
