"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import {
  getRoutingPreview,
  autoPromoteRoutingMatrixNow,
  type RoutingPreviewEntry,
} from "../../../lib/api/models";
import {
  BEHAVIOR_MODES,
  ROUTING_INTENTS,
  labelProvider,
  type RoutingConfigState,
} from "./shared";

// Modalita' statiche per cui esiste una matrice in nexus_routing_matrix.
// "dinamico" e "manuale" non hanno una preview (il primo risolve a runtime).
const PREVIEW_MODES = new Set(["veloce", "economica", "bilanciata", "approfondita"]);

function intentLabel(intent: string): string {
  return ROUTING_INTENTS.find((i) => i.key === intent)?.label ?? intent;
}

interface BehaviorModeSectionProps {
  config: RoutingConfigState;
  setConfig: React.Dispatch<React.SetStateAction<RoutingConfigState>>;
  behaviorSaved: boolean;
}

export function BehaviorModeSection({ config, setConfig, behaviorSaved }: BehaviorModeSectionProps) {
  const tc = useThemeColors();
  const [previewByMode, setPreviewByMode] = useState<Record<string, RoutingPreviewEntry[]>>({});
  const [avgByMode, setAvgByMode] = useState<Record<string, number>>({});
  const [previewStatus, setPreviewStatus] = useState<"idle" | "loading" | "error">("idle");
  const [promoteBusy, setPromoteBusy] = useState(false);
  const [promoteMsg, setPromoteMsg] = useState<string | null>(null);

  const mode = config.behaviorMode;

  async function loadPreview(m: string, force = false) {
    if (!PREVIEW_MODES.has(m)) return;
    if (!force && previewByMode[m]) return;
    setPreviewStatus("loading");
    try {
      const res = await getRoutingPreview(m);
      setPreviewByMode((p) => ({ ...p, [m]: res.routing ?? [] }));
      setAvgByMode((a) => ({ ...a, [m]: res.estimatedAvgCostInputPerMillion ?? 0 }));
      setPreviewStatus(res.error ? "error" : "idle");
    } catch {
      setPreviewStatus("error");
    }
  }

  useEffect(() => {
    void loadPreview(mode);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode]);

  async function handleAutoPromote() {
    setPromoteBusy(true);
    setPromoteMsg(null);
    try {
      const res = await autoPromoteRoutingMatrixNow();
      if (res.error) {
        setPromoteMsg(`Errore: ${res.error}`);
      } else {
        setPromoteMsg(
          `Aggiornate ${res.updated ?? 0} celle (${res.skipped_manual ?? 0} manuali saltate). ` +
            "La preview puo' impiegare fino a 60s a riflettere le promozioni.",
        );
        await loadPreview(mode, true);
      }
    } catch (e) {
      setPromoteMsg(`Errore: ${e instanceof Error ? e.message : "auto-promote fallito"}`);
    } finally {
      setPromoteBusy(false);
    }
  }

  const preview = previewByMode[mode] ?? [];

  return (
    <div className="card-sm" style={{ background: tc.bgHover }}>
      <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
        <div>
          <div className="text-base font-bold">Modalita' Nexus</div>
          <div className="text-sm text-muted" style={{ marginTop: 3 }}>
            {BEHAVIOR_MODES.find((m) => m.value === mode)?.desc}
          </div>
        </div>
        {behaviorSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato</span>}
      </div>
      <div className="flex-row-gap-8" style={{ flexWrap: "wrap" }}>
        {BEHAVIOR_MODES.map((m) => (
          <button
            key={m.value}
            onClick={() => setConfig((c) => ({ ...c, behaviorMode: m.value }))}
            title={m.desc}
            className={`btn ${mode === m.value ? "btn-primary" : "btn-secondary"}`}
            style={{
              border: `1px solid ${mode === m.value ? tc.accent : tc.border}`,
              background: mode === m.value ? tc.accent : tc.bgInput,
              color: mode === m.value ? "#fff" : tc.text,
            }}
          >
            {m.label}
          </button>
        ))}
      </div>

      {/* Preview REALE dalla matrice DB corrente (nexus_routing_matrix) */}
      {PREVIEW_MODES.has(mode) && (
        <div style={{ marginTop: 14 }}>
          <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 8, gap: 8 }}>
            <div style={{ fontSize: 12, fontWeight: 600, color: "var(--color-textSecondary)" }}>
              Routing reale dalla matrice corrente
              {avgByMode[mode] != null && ` — costo input medio ~$${avgByMode[mode].toFixed(2)}/M`}
            </div>
            <button
              onClick={() => void handleAutoPromote()}
              disabled={promoteBusy}
              className="btn btn-secondary"
              style={{
                padding: "5px 10px",
                border: `1px solid ${tc.border}`,
                background: promoteBusy ? tc.bgInput : tc.bgCard,
                color: tc.text,
                fontSize: 11,
                cursor: promoteBusy ? "wait" : "pointer",
                whiteSpace: "nowrap",
              }}
              title="Ricalcola le celle best-fit della matrice dal catalog corrente (celle manuali saltate)"
            >
              {promoteBusy ? "..." : "Auto-promuovi ora"}
            </button>
          </div>
          {promoteMsg && (
            <div style={{ fontSize: 11, color: promoteMsg.startsWith("Errore") ? tc.error : tc.success, marginBottom: 8 }}>
              {promoteMsg}
            </div>
          )}
          {previewStatus === "loading" && preview.length === 0 && (
            <div style={{ fontSize: 11, color: tc.textMuted }}>Caricamento routing...</div>
          )}
          {previewStatus === "error" && (
            <div style={{ fontSize: 11, color: tc.error }}>
              Matrice di routing non disponibile per questa modalita'.
            </div>
          )}
          {previewStatus === "idle" && preview.length === 0 && (
            <div style={{ fontSize: 11, color: tc.textMuted }}>
              Nessuna cella nella matrice per questa modalita'. Usa &quot;Auto-promuovi ora&quot; per popolarla dal catalog.
            </div>
          )}
          <div style={{ display: "grid", gap: 3 }}>
            {preview.map((entry, i) => (
              <div
                key={`${entry.intent}-${i}`}
                style={{
                  display: "grid",
                  gridTemplateColumns: "130px 1fr auto",
                  gap: 8,
                  alignItems: "center",
                  padding: "5px 8px",
                  borderRadius: 6,
                  background: "var(--color-bgInput)",
                }}
              >
                <span style={{ fontSize: 11, color: "var(--color-textMuted)", fontWeight: 600 }}>
                  {intentLabel(entry.intent)}
                </span>
                <span style={{ fontSize: 12 }}>
                  <span style={{ color: "var(--color-accent)", fontWeight: 600 }}>{labelProvider(entry.provider)}</span>
                  <span style={{ color: "var(--color-textMuted)" }}> / </span>
                  <span style={{ color: "var(--color-text)", fontFamily: "var(--font-mono)", fontSize: 11 }}>{entry.model}</span>
                </span>
                <span style={{ fontSize: 10, color: "var(--color-textMuted)", whiteSpace: "nowrap" }}>
                  ${entry.inputCost.toFixed(2)}/M · {entry.speed}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Preview per modalita' Dinamico */}
      {mode === "dinamico" && (
        <div style={{
          marginTop: 14, padding: "10px 12px", borderRadius: 8,
          background: "var(--color-bgInput)", border: "1px solid var(--color-border)",
        }}>
          <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 6 }}>Come funziona il routing Dinamico</div>
          <div style={{ fontSize: 12, color: "var(--color-textMuted)", lineHeight: 1.6 }}>
            Per ogni messaggio l&apos;orchestratore analizza:
            <br />1. <strong>Intent</strong> (fix / chat / architettura...) e <strong>complessita&apos;</strong> (token stimati)
            <br />2. Determina il <strong>tier richiesto</strong> (light / medium / high / heavy / frontier) e la <strong>capability</strong> necessaria
            <br />3. Interroga il <strong>catalogo modelli</strong> ordinando per costo — sceglie il piu&apos; economico che soddisfa i requisiti
            <br />4. Se il catalogo e&apos; vuoto, usa la matrice Bilanciata come fallback
          </div>
        </div>
      )}
    </div>
  );
}
