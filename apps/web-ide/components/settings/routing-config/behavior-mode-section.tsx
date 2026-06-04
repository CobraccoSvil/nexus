"use client";

import { useThemeColors } from "../../../lib/theme";
import {
  BEHAVIOR_MODES,
  NEXUS_ROUTING_MATRIX,
  labelProvider,
  type RoutingConfigState,
} from "./shared";

interface BehaviorModeSectionProps {
  config: RoutingConfigState;
  setConfig: React.Dispatch<React.SetStateAction<RoutingConfigState>>;
  behaviorSaved: boolean;
}

export function BehaviorModeSection({ config, setConfig, behaviorSaved }: BehaviorModeSectionProps) {
  const tc = useThemeColors();

  return (
    <div className="card-sm" style={{ background: tc.bgHover }}>
      <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
        <div>
          <div className="text-base font-bold">Modalità Nexus</div>
          <div className="text-sm text-muted" style={{ marginTop: 3 }}>
            {BEHAVIOR_MODES.find((m) => m.value === config.behaviorMode)?.desc}
          </div>
        </div>
        {behaviorSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato ✓</span>}
      </div>
      <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
        {BEHAVIOR_MODES.map((mode) => (
          <button
            key={mode.value}
            onClick={() => setConfig((c) => ({ ...c, behaviorMode: mode.value }))}
            title={mode.desc}
            className={`btn ${config.behaviorMode === mode.value ? "btn-primary" : "btn-secondary"}`}
            style={{
              border: `1px solid ${config.behaviorMode === mode.value ? tc.accent : tc.border}`,
              background: config.behaviorMode === mode.value ? tc.accent : tc.bgInput,
              color: config.behaviorMode === mode.value ? "#fff" : tc.text,
            }}
          >
            {mode.label}
          </button>
        ))}
      </div>

      {/* Preview per modalità con matrice statica */}
      {NEXUS_ROUTING_MATRIX[config.behaviorMode] && (
        <div style={{ marginTop: 14 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: "var(--color-textSecondary)", marginBottom: 8 }}>
            Routing per questa modalità — scala automaticamente in base a intent e complessità:
          </div>
          <div style={{ display: "grid", gap: 3 }}>
            {NEXUS_ROUTING_MATRIX[config.behaviorMode].map((entry, i) => (
              <div key={i} style={{
                display: "grid",
                gridTemplateColumns: "130px 1fr auto",
                gap: 8,
                alignItems: "center",
                padding: "5px 8px",
                borderRadius: 6,
                background: "var(--color-bgInput)",
              }}>
                <span style={{ fontSize: 11, color: "var(--color-textMuted)", fontWeight: 600 }}>{entry.label}</span>
                <span style={{ fontSize: 12 }}>
                  <span style={{ color: "var(--color-accent)", fontWeight: 600 }}>{labelProvider(entry.provider)}</span>
                  <span style={{ color: "var(--color-textMuted)" }}> / </span>
                  <span style={{ color: "var(--color-text)", fontFamily: "monospace", fontSize: 11 }}>{entry.model}</span>
                </span>
                {entry.tokens && (
                  <span style={{ fontSize: 10, color: "var(--color-textMuted)", whiteSpace: "nowrap" }}>{entry.tokens}</span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Preview per modalità Dinamico */}
      {config.behaviorMode === "dinamico" && (
        <div style={{
          marginTop: 14, padding: "10px 12px", borderRadius: 8,
          background: "var(--color-bgInput)", border: "1px solid var(--color-border)",
        }}>
          <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6 }}>🤖 Come funziona il routing Dinamico</div>
          <div style={{ fontSize: 12, color: "var(--color-textMuted)", lineHeight: 1.6 }}>
            Per ogni messaggio l&apos;orchestratore analizza:
            <br />① <strong>Intent</strong> (fix / chat / architettura…) e <strong>complessità</strong> (token stimati)
            <br />② Determina il <strong>tier richiesto</strong> (light / medium / heavy) e la <strong>capability</strong> necessaria
            <br />③ Interroga il <strong>catalogo modelli</strong> ordinando per costo — sceglie il più economico che soddisfa i requisiti
            <br />④ Se il catalogo è vuoto, usa la matrice Bilanciata come fallback
          </div>
        </div>
      )}
    </div>
  );
}
