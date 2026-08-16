"use client";

import { useTheme, useThemeColors } from "../../../lib/theme";
import {
  PURPOSE_KEYS,
  PURPOSE_TIER_OPTIONS,
  inputStyle,
  type RoutingConfigState,
} from "./shared";
import { useI18n } from "../../../lib/i18n";

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

/**
 * Pannello dei purpose model, TIER-ONLY (mig 0723).
 *
 * I due select provider/modello sono stati rimossi col pin statico: mostravano
 * una configurazione che il resolver ignorava (misurato il 2026-07-16: figure
 * dichiarate deepseek giravano su groq). Qui si sceglie la FASCIA; chi risponde
 * davvero lo dice `resolved`, chiesto al resolver — lo stesso codice che decide
 * durante un run (regola L), quindi il pannello non puo' divergere dall'effetto.
 */
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
  const { t } = useI18n();
  const tc = useThemeColors();
  const { resolved } = useTheme();

  return (
    <div className="card-sm" style={{ background: tc.bgHover }}>
      <div className="flex-row" style={{ justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
        <div>
          <div className="text-base font-bold">{t("settings.purposeModels")}</div>
          <div className="text-sm text-muted" style={{ marginTop: 3 }}>
            Modelli per task interni (non routing utente). Includono il fallback automatico su loop tool-use.
          </div>
        </div>
        {purposeLoading && <span className="text-sm text-muted">{t("settings.caricamento2")}</span>}
        {purposeSaved && <span className="text-sm font-semibold" style={{ color: tc.success }}>{t("settings.salvato2")}</span>}
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
          const pm = config.purposeModels[p.key] ?? {
            notes: null,
            tier: null,
            required_capability: null,
            requires_tool_use: false,
            resolved: null,
          };
          const currentTier = pm.tier ?? "";
          const savingThis = !!purposeSaving[p.key];
          const testBusy = !!purposeTestBusy[p.key];
          const testMsg = purposeTestMsg[p.key];
          return (
            <div key={p.key} style={{
              display: "grid",
              gridTemplateColumns: "220px 150px 1fr auto auto",
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
                title={t("settings.categoriaModelloTierStatico")}
              >
                {/* Riga storica senza tier: si mostra vuoto finche' l'admin
                    non sceglie una fascia (il salvataggio la pretende). */}
                {currentTier === "" && <option value="">{"—"}</option>}
                {PURPOSE_TIER_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>

              <div style={{ fontSize: 11, color: tc.textMuted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {pm.resolved ? (
                  <span title={pm.resolved.rationale}>
                    {"→"} {pm.resolved.provider}/{pm.resolved.model}
                  </span>
                ) : currentTier !== "" ? (
                  <span style={{ color: tc.error }} title="Nessun modello risolvibile ora: catalog, gate di qualificazione o provider in cooldown">
                    {"→"} non risolvibile ora
                  </span>
                ) : (
                  <span style={{ color: tc.error }}>senza tier: non risolvibile</span>
                )}
              </div>

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
                title={t("settings.salvaQuestoPurposeModel")}
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
                  title={t("settings.testaLaRisoluzioneRuntime")}
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
