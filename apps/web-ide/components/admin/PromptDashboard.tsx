"use client";

/**
 * PromptDashboard — riepilogo metriche prompt ultimi 7 giorni (Fase 4).
 *
 * Mostra per ogni prompt agente attivo:
 * - Schema type (xml v2 / plain legacy)
 * - Avg reflection score (7gg)
 * - Feedback positivi (%)
 * - Badge "SPERIMENTALE" se experimental=true
 */

import { useEffect, useState, useCallback } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  getPromptDashboard,
  type PromptDashboardData,
  type PromptDashboardEntry,
} from "../../lib/api-client";

function ScoreBadge({ score }: { score?: number }) {
  if (score === undefined || score === null) {
    return <span style={{ color: "#9ca3af", fontSize: 12 }}>--</span>;
  }
  const pct = Math.round(score * 100);
  const bg = score >= 0.85 ? "#dcfce7" : score >= 0.65 ? "#fef9c3" : "#fee2e2";
  const fg = score >= 0.85 ? "#166534" : score >= 0.65 ? "#854d0e" : "#991b1b";
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        padding: "2px 8px",
        borderRadius: 4,
        fontSize: 12,
        fontWeight: 500,
        background: bg,
        color: fg,
      }}
    >
      {pct}%
    </span>
  );
}

function MiniBar({ value, max = 1 }: { value?: number; max?: number }) {
  if (value === undefined || value === null) {
    return <div style={{ width: "100%", height: 6, background: "#f3f4f6", borderRadius: 3 }} />;
  }
  const pct = Math.min(100, Math.round((value / max) * 100));
  const color = pct >= 85 ? "#22c55e" : pct >= 65 ? "#facc15" : "#f87171";
  return (
    <div style={{ width: "100%", height: 6, background: "#f3f4f6", borderRadius: 3, overflow: "hidden" }}>
      <div style={{ height: "100%", width: `${pct}%`, background: color, transition: "width 0.3s" }} />
    </div>
  );
}

export default function PromptDashboard() {
  const tc = useThemeColors();
  const [data, setData] = useState<PromptDashboardData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<"all" | "xml" | "warning">("all");

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const d = await getPromptDashboard();
      setData(d);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento dati");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    const interval = setInterval(load, 60000);
    return () => clearInterval(interval);
  }, [load]);

  const prompts = data?.prompts ?? [];
  const filtered = prompts.filter((p) => {
    if (filter === "xml") return p.schema_type === "xml";
    if (filter === "warning")
      return (p.avg_reflection_score ?? 1) < 0.65 || p.experimental;
    return true;
  });

  const globalAvg = data?.global_reflection_avg_7d;

  const cardStyle: React.CSSProperties = {
    background: tc.bgCard,
    border: `1px solid ${tc.border}`,
    borderRadius: 8,
    padding: "16px 20px",
  };

  const filterBtnStyle = (active: boolean): React.CSSProperties => ({
    padding: "6px 14px",
    fontSize: 13,
    borderRadius: 6,
    border: `1px solid ${active ? tc.accent : tc.border}`,
    background: active ? tc.accent : tc.bgCard,
    color: active ? "#fff" : tc.text,
    cursor: "pointer",
    fontWeight: active ? 600 : 400,
  });

  const thStyle: React.CSSProperties = {
    padding: "10px 14px",
    textAlign: "left" as const,
    fontWeight: 500,
    fontSize: 12,
    color: tc.textSecondary,
    borderBottom: `1px solid ${tc.border}`,
  };

  const tdStyle: React.CSSProperties = {
    padding: "10px 14px",
    fontSize: 13,
    borderBottom: `1px solid ${tc.border}`,
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
      {/* Header metriche globali */}
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: 16 }}>
        <div style={cardStyle}>
          <div style={{ fontSize: 11, color: tc.textSecondary, textTransform: "uppercase", letterSpacing: "0.05em" }}>
            Prompt attivi
          </div>
          <div style={{ fontSize: 24, fontWeight: 700, marginTop: 4, color: tc.text }}>
            {data?.total_prompts ?? "--"}
          </div>
        </div>
        <div style={cardStyle}>
          <div style={{ fontSize: 11, color: tc.textSecondary, textTransform: "uppercase", letterSpacing: "0.05em" }}>
            Reflection score medio (7gg)
          </div>
          <div style={{ fontSize: 24, fontWeight: 700, marginTop: 4 }}>
            <ScoreBadge score={globalAvg} />
          </div>
        </div>
        <div style={cardStyle}>
          <div style={{ fontSize: 11, color: tc.textSecondary, textTransform: "uppercase", letterSpacing: "0.05em" }}>
            Esperimenti running
          </div>
          <div style={{ fontSize: 24, fontWeight: 700, marginTop: 4, color: tc.accent }}>
            {data?.running_experiments ?? "--"}
          </div>
        </div>
        <div style={cardStyle}>
          <div style={{ fontSize: 11, color: tc.textSecondary, textTransform: "uppercase", letterSpacing: "0.05em" }}>
            Schema XML v2
          </div>
          <div style={{ fontSize: 24, fontWeight: 700, marginTop: 4, color: "#a855f7" }}>
            {prompts.filter((p) => p.schema_type === "xml").length}
          </div>
        </div>
      </div>

      {/* Filtri + Refresh */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <div style={{ display: "flex", gap: 8 }}>
          {(["all", "xml", "warning"] as const).map((f) => (
            <button
              key={f}
              onClick={() => setFilter(f)}
              style={filterBtnStyle(filter === f)}
            >
              {f === "all" ? "Tutti" : f === "xml" ? "Schema XML" : "Sotto soglia"}
            </button>
          ))}
        </div>
        <button
          onClick={load}
          disabled={loading}
          style={{
            fontSize: 13,
            color: tc.accent,
            background: "transparent",
            border: "none",
            cursor: loading ? "not-allowed" : "pointer",
            opacity: loading ? 0.5 : 1,
            textDecoration: "underline",
          }}
        >
          {loading ? "Caricamento..." : "Aggiorna"}
        </button>
      </div>

      {error && (
        <div
          style={{
            background: "#fef2f2",
            border: "1px solid #fecaca",
            color: "#b91c1c",
            fontSize: 13,
            borderRadius: 8,
            padding: "10px 14px",
          }}
        >
          {error}
        </div>
      )}

      {/* Tabella prompt */}
      <div style={{ background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 8, overflow: "hidden" }}>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
          <thead>
            <tr style={{ background: tc.bgHeader }}>
              <th style={thStyle}>Chiave</th>
              <th style={thStyle}>Versione</th>
              <th style={thStyle}>Schema</th>
              <th style={thStyle}>Reflection (7gg)</th>
              <th style={thStyle}>Feedback +</th>
              <th style={thStyle}>Run</th>
            </tr>
          </thead>
          <tbody>
            {loading && filtered.length === 0 ? (
              <tr>
                <td colSpan={6} style={{ ...tdStyle, textAlign: "center", padding: "32px 14px", color: tc.textSecondary }}>
                  Caricamento...
                </td>
              </tr>
            ) : filtered.length === 0 ? (
              <tr>
                <td colSpan={6} style={{ ...tdStyle, textAlign: "center", padding: "32px 14px", color: tc.textSecondary }}>
                  Nessun prompt corrisponde al filtro
                </td>
              </tr>
            ) : (
              filtered.map((p) => (
                <PromptRow key={`${p.prompt_key}-${p.prompt_version}`} prompt={p} tc={tc} tdStyle={tdStyle} />
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function PromptRow({
  prompt: p,
  tc,
  tdStyle,
}: {
  prompt: PromptDashboardEntry;
  tc: ReturnType<typeof useThemeColors>;
  tdStyle: React.CSSProperties;
}) {
  const isWarning = (p.avg_reflection_score ?? 1) < 0.65;
  return (
    <tr style={isWarning ? { background: "#fef2f2" } : undefined}>
      <td style={{ ...tdStyle, fontFamily: "monospace", fontSize: 12, color: tc.text }}>
        <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
          {p.prompt_key}
          {p.experimental && (
            <span
              style={{
                padding: "2px 6px",
                fontSize: 10,
                borderRadius: 4,
                background: "#fef3c7",
                color: "#92400e",
                fontWeight: 500,
              }}
            >
              SPERIM.
            </span>
          )}
        </div>
      </td>
      <td style={{ ...tdStyle, color: tc.textSecondary }}>v{p.prompt_version}</td>
      <td style={tdStyle}>
        {p.schema_type === "xml" ? (
          <span
            style={{
              padding: "2px 6px",
              fontSize: 11,
              borderRadius: 4,
              background: "#dbeafe",
              color: "#1e40af",
              fontWeight: 500,
            }}
          >
            XML v2
          </span>
        ) : (
          <span
            style={{
              padding: "2px 6px",
              fontSize: 11,
              borderRadius: 4,
              background: "#f3f4f6",
              color: "#6b7280",
            }}
          >
            plain
          </span>
        )}
      </td>
      <td style={{ ...tdStyle, width: 160 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <ScoreBadge score={p.avg_reflection_score} />
          <div style={{ flex: 1 }}>
            <MiniBar value={p.avg_reflection_score} />
          </div>
        </div>
      </td>
      <td style={tdStyle}>
        {p.feedback_count > 0 ? (
          <span style={{ color: tc.text }}>
            {Math.round((p.feedback_positive_rate ?? 0) * 100)}%
            <span style={{ color: tc.textSecondary, marginLeft: 4, fontSize: 11 }}>
              ({p.feedback_count})
            </span>
          </span>
        ) : (
          <span style={{ color: tc.textSecondary, fontSize: 12 }}>--</span>
        )}
      </td>
      <td style={{ ...tdStyle, color: tc.textSecondary }}>{p.reflection_runs}</td>
    </tr>
  );
}
