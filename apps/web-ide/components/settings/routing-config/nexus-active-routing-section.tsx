"use client";

import { useTheme, useThemeColors } from "../../../lib/theme";

interface NexusActiveRoutingSectionProps {
  nexusRoutingPct: number;
  setNexusRoutingPct: React.Dispatch<React.SetStateAction<number>>;
  nexusPctSaving: boolean;
  nexusPctSaved: boolean;
  nexusPctError: string | null;
  saveNexusPct: () => void;
}

export function NexusActiveRoutingSection({
  nexusRoutingPct,
  setNexusRoutingPct,
  nexusPctSaving,
  nexusPctSaved,
  nexusPctError,
  saveNexusPct,
}: NexusActiveRoutingSectionProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();

  return (
    <div className="card">
      <div className="flex-row" style={{ gap: 8, marginBottom: 4 }}>
        <span className="text-xl font-bold">🧠 Nexus Active Routing (Q-Learning)</span>
        {nexusRoutingPct > 0 && (
          <span style={{
            fontSize: 10,
            fontWeight: 700,
            background: "#6366f120",
            color: "#6366f1",
            border: "1px solid #6366f140",
            borderRadius: 6,
            padding: "1px 7px",
          }}>
            {nexusRoutingPct}% ATTIVO
          </span>
        )}
      </div>
      <div className="text-sm text-muted" style={{ marginBottom: 14 }}>
        Percentuale di richieste chat gestite dal router Q-Learning di Nexus anziché dal routing classico.
        Il router seleziona automaticamente l&apos;agent type ottimale (Coder, Tester, Architect…)
        basandosi su 60+ tipi di agente e dati storici di performance.
        Imposta 0% per disabilitare, 100% per routing Q-Learning completo.
      </div>

      {/* Preset rapidi */}
      <div className="flex-row" style={{ gap: 6, marginBottom: 12, flexWrap: "wrap" }}>
        {[0, 10, 25, 50, 75, 100].map((pct) => (
          <button
            key={pct}
            onClick={() => setNexusRoutingPct(pct)}
            style={{
              padding: "3px 12px",
              borderRadius: 6,
              border: `1px solid ${nexusRoutingPct === pct ? "#6366f1" : tc.border}`,
              background: nexusRoutingPct === pct ? "#6366f120" : tc.bgInput,
              color: nexusRoutingPct === pct ? "#6366f1" : tc.textSecondary,
              fontSize: 11,
              cursor: "pointer",
              fontWeight: nexusRoutingPct === pct ? 700 : 400,
            }}
          >
            {pct === 0 ? "Off" : `${pct}%`}
          </button>
        ))}
      </div>

      {/* Slider + valore numerico */}
      <div className="flex-row" style={{ gap: 12, marginBottom: 14 }}>
        <input
          type="range"
          min={0}
          max={100}
          step={5}
          value={nexusRoutingPct}
          onChange={(e) => setNexusRoutingPct(parseInt(e.target.value, 10))}
          style={{ flex: 1, accentColor: "#6366f1", cursor: "pointer" }}
        />
        <span style={{ fontSize: 13, fontWeight: 700, color: "#6366f1", minWidth: 36, textAlign: "right" }}>
          {nexusRoutingPct}%
        </span>
        <input
          type="number"
          min={0}
          max={100}
          value={nexusRoutingPct}
          onChange={(e) => setNexusRoutingPct(Math.max(0, Math.min(100, parseInt(e.target.value, 10) || 0)))}
          style={{
            width: 64,
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

      <button
        onClick={saveNexusPct}
        disabled={nexusPctSaving}
        style={{
          padding: "6px 18px",
          borderRadius: 8,
          border: "none",
          background: nexusPctSaved ? "#22c55e" : "#6366f1",
          color: "#fff",
          fontWeight: 600,
          fontSize: 13,
          cursor: nexusPctSaving ? "wait" : "pointer",
          opacity: nexusPctSaving ? 0.7 : 1,
        }}
      >
        {nexusPctSaving ? "Salvo..." : nexusPctSaved ? "✓ Salvato" : "Salva routing Nexus"}
      </button>
      {nexusPctError && (
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
          {nexusPctError}
        </div>
      )}
    </div>
  );
}
