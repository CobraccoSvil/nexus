"use client";

import { useTheme, useThemeColors } from "../../../lib/theme";

interface ParallelAgentsSectionProps {
  parallelEnabled: boolean;
  setParallelEnabled: React.Dispatch<React.SetStateAction<boolean>>;
  parallelMax: number;
  setParallelMax: React.Dispatch<React.SetStateAction<number>>;
  parallelSaving: boolean;
  parallelSaved: boolean;
  parallelError: string | null;
  saveParallelSettings: () => void;
}

export function ParallelAgentsSection({
  parallelEnabled,
  setParallelEnabled,
  parallelMax,
  setParallelMax,
  parallelSaving,
  parallelSaved,
  parallelError,
  saveParallelSettings,
}: ParallelAgentsSectionProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();

  return (
    <div className="card">
      <div className="flex-row" style={{ gap: 8, marginBottom: 4 }}>
        <span className="text-xl font-bold">⚡ Agenti Paralleli</span>
        {parallelEnabled && (
          <span style={{
            fontSize: 10,
            fontWeight: 700,
            background: "#22c55e20",
            color: "#22c55e",
            border: "1px solid #22c55e40",
            borderRadius: 6,
            padding: "1px 7px",
          }}>
            ATTIVO
          </span>
        )}
      </div>
      <div className="text-sm text-muted" style={{ marginBottom: 14 }}>
        Permette all&apos;orchestratore di lanciare più agenti contemporaneamente per accelerare task complessi.
        L&apos;agente può usare i tool <code style={{ fontFamily: "monospace", fontSize: 11 }}>dispatch_subagent</code> / <code style={{ fontFamily: "monospace", fontSize: 11 }}>dispatch_subagents</code> per delegare sotto-task a sub-agenti specializzati in parallelo.
      </div>

      <div className="flex-row" style={{ gap: 12, marginBottom: 12 }}>
        {/* Toggle */}
        <button
          onClick={() => setParallelEnabled((v) => !v)}
          style={{
            width: 44,
            height: 24,
            borderRadius: 12,
            border: "none",
            background: parallelEnabled ? tc.accent : tc.border,
            cursor: "pointer",
            position: "relative",
            transition: "background 0.2s",
            flexShrink: 0,
          }}
        >
          <span style={{
            position: "absolute",
            top: 3,
            left: parallelEnabled ? 22 : 3,
            width: 18,
            height: 18,
            borderRadius: "50%",
            background: "#fff",
            transition: "left 0.2s",
            boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
          }} />
        </button>
        <span className="text-base" style={{ color: tc.text }}>
          {parallelEnabled ? "Abilitato" : "Disabilitato"}
        </span>
      </div>

      {parallelEnabled && (
        <div className="flex-row" style={{ gap: 10, marginBottom: 12 }}>
          <label className="text-sm" style={{ color: "var(--color-textSecondary)", minWidth: 160 }}>
            Max agenti paralleli (1–8):
          </label>
          <input
            type="number"
            min={1}
            max={8}
            value={parallelMax}
            onChange={(e) => setParallelMax(parseInt(e.target.value, 10) || 1)}
            style={{
              width: 60,
              padding: "4px 8px",
              borderRadius: 6,
              border: "1px solid var(--color-border)",
              background: "var(--color-bgInput)",
              color: "var(--color-text)",
              fontSize: 13,
              textAlign: "center",
            }}
          />
        </div>
      )}

      <button
        onClick={saveParallelSettings}
        disabled={parallelSaving}
        style={{
          padding: "6px 18px",
          borderRadius: 8,
          border: "none",
          background: parallelSaved ? "#22c55e" : tc.accent,
          color: "#fff",
          fontWeight: 600,
          fontSize: 13,
          cursor: parallelSaving ? "wait" : "pointer",
          opacity: parallelSaving ? 0.7 : 1,
        }}
      >
        {parallelSaving ? "Salvo..." : parallelSaved ? "✓ Salvato" : "Salva impostazioni agenti"}
      </button>
      {parallelError && (
        <div
          style={{
            marginTop: 10,
            padding: "8px 10px",
            borderRadius: 8,
            border: `1px solid ${tc.error}`,
            background: resolved === "dark" ? "#2d1215" : "#fef2f2",
            color: "var(--color-error)",
            fontSize: 12,
          }}
        >
          {parallelError}
        </div>
      )}
    </div>
  );
}
