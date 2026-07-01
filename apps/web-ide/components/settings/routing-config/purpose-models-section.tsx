"use client";

import { useTheme, useThemeColors } from "../../../lib/theme";
import {
  PROVIDER_MODELS,
  PROVIDERS,
  PURPOSE_KEYS,
  PURPOSE_TIER_OPTIONS,
  inputStyle,
  labelProvider,
  type ProviderName,
  type RoutingConfigState,
} from "./shared";

interface PurposeModelsSectionProps {
  config: RoutingConfigState;
  setConfig: React.Dispatch<React.SetStateAction<RoutingConfigState>>;
  purposeLoading: boolean;
  purposeSaved: boolean;
  purposeError: string | null;
  purposeSaving: Record<string, boolean>;
  purposeTestBusy: Record<string, boolean>;
  purposeTestMsg: Record<string, string>;
  savePurposeModel: (purpose: string) => void;
  testPurposeModel: (purpose: string) => void;
}

export function PurposeModelsSection({
  config,
  setConfig,
  purposeLoading,
  purposeSaved,
  purposeError,
  purposeSaving,
  purposeTestBusy,
  purposeTestMsg,
  savePurposeModel,
  testPurposeModel,
}: PurposeModelsSectionProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();

  return (
    <div className="card-sm" style={{ background: tc.bgHover }}>
      <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
        <div>
          <div className="text-base font-bold">Purpose models</div>
          <div className="text-sm text-muted" style={{ marginTop: 3 }}>
            Modelli per task interni (non routing utente). Includono il fallback automatico su loop tool-use.
          </div>
        </div>
        {purposeLoading && <span className="text-sm text-muted">Caricamento…</span>}
        {purposeSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>Salvato ✓</span>}
      </div>
      {purposeError && (
        <div style={{
          padding: "8px 10px",
          borderRadius: 8,
          border: `1px solid ${tc.error}`,
          background: resolved === "dark" ? "#2d1215" : "#fef2f2",
          color: "var(--color-error)",
          marginBottom: 10,
          fontSize: 12,
        }}>
          {purposeError}
        </div>
      )}
      <div style={{ display: "grid", gap: 10 }}>
        {PURPOSE_KEYS.map((p) => {
          const pm = config.purposeModels[p.key] ?? { provider: "anthropic" as ProviderName, model_id: PROVIDER_MODELS.anthropic[0], notes: null, tier: null };
          const currentTier = pm.tier ?? "";
          const tierActive = currentTier !== "";
          const savingThis = !!purposeSaving[p.key];
          const testBusy = !!purposeTestBusy[p.key];
          const testMsg = purposeTestMsg[p.key];
          return (
            <div key={p.key} style={{
              display: "grid",
              gridTemplateColumns: "170px 150px 160px 1fr auto auto",
              gap: 10,
              alignItems: "center",
              padding: "8px 10px",
              borderRadius: 8,
              border: `1px solid ${tc.border}`,
              background: "var(--color-bgInput)",
            }}>
              <div style={{ minWidth: 0 }}>
                <div style={{ fontSize: 12, fontWeight: 700, color: tc.text, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {p.label}
                </div>
                <div style={{ fontSize: 11, color: tc.textMuted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {p.desc}
                </div>
                <div style={{ fontSize: 10, color: tc.textMuted, marginTop: 2, fontFamily: "var(--font-mono)" }}>{p.key}</div>
                {tierActive && (
                  <div style={{ marginTop: 4, fontSize: 10, color: tc.textMuted }}>
                    Selezione dinamica dal catalog per categoria. Provider/modello sono usati solo come fallback statico.
                  </div>
                )}
                {!!testMsg && (
                  <div style={{ marginTop: 4, fontSize: 10, color: testMsg.startsWith("OK:") ? tc.success : tc.error }}>
                    {testMsg}
                  </div>
                )}
              </div>

              <select
                value={currentTier}
                onChange={(e) => {
                  const raw = e.target.value;
                  const tier = raw === "" ? null : raw;
                  setConfig((c) => ({
                    ...c,
                    purposeModels: {
                      ...c.purposeModels,
                      [p.key]: { ...pm, tier },
                    },
                  }));
                }}
                style={{ ...inputStyle(tc), padding: "6px 8px", fontSize: 12 }}
                title="Categoria modello (tier). Statico = modello fisso scelto manualmente."
              >
                {PURPOSE_TIER_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>

              <select
                value={pm.provider}
                onChange={(e) => {
                  const provider = e.target.value as ProviderName;
                  const firstModel = PROVIDER_MODELS[provider]?.[0] ?? "";
                  setConfig((c) => ({
                    ...c,
                    purposeModels: {
                      ...c.purposeModels,
                      [p.key]: { ...pm, provider, model_id: firstModel || pm.model_id },
                    },
                  }));
                }}
                style={{ ...inputStyle(tc), padding: "6px 8px", fontSize: 12 }}
              >
                {PROVIDERS.map((prov) => (
                  <option key={prov} value={prov}>{labelProvider(prov)}</option>
                ))}
              </select>

              <select
                value={pm.model_id}
                onChange={(e) => {
                  const model_id = e.target.value;
                  setConfig((c) => ({
                    ...c,
                    purposeModels: { ...c.purposeModels, [p.key]: { ...pm, model_id } },
                  }));
                }}
                style={{ ...inputStyle(tc), padding: "6px 8px", fontSize: 12 }}
              >
                {(PROVIDER_MODELS[pm.provider] ?? []).map((m) => (
                  <option key={m} value={m}>{m}</option>
                ))}
              </select>

              <button
                onClick={() => void savePurposeModel(p.key)}
                disabled={savingThis}
                className="btn btn-secondary"
                style={{
                  padding: "6px 10px",
                  background: savingThis ? tc.bgInput : tc.bgCard,
                  color: tc.text,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 8,
                  cursor: savingThis ? "wait" : "pointer",
                  fontSize: 12,
                  whiteSpace: "nowrap",
                }}
                title="Salva questo purpose model"
              >
                {savingThis ? "…" : "Salva"}
              </button>

              {p.key === "loop_fallback_default" ? (
                <button
                  onClick={() => void testPurposeModel(p.key)}
                  disabled={testBusy}
                  className="btn btn-secondary"
                  style={{
                    padding: "6px 10px",
                    background: testBusy ? tc.bgInput : tc.bgCard,
                    color: tc.text,
                    border: `1px solid ${tc.border}`,
                    borderRadius: 8,
                    cursor: testBusy ? "wait" : "pointer",
                    fontSize: 12,
                    whiteSpace: "nowrap",
                  }}
                  title="Testa la risoluzione runtime del fallback su loop"
                >
                  {testBusy ? "…" : "Test"}
                </button>
              ) : (
                <div />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
