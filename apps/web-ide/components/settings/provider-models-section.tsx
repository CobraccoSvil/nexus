"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { AutoWidthSelect } from "../auto-width-select";
import {
  getProviderModelsAdmin,
  setModelEnabled,
  setModelTier,
  type ModelCatalogEntry,
  type PerformanceTier,
} from "../../lib/api/models";

/** La scala del catalog (mig 0528/0547). Specchio di PERFORMANCE_TIERS in
 *  nexus-agent-graph/src/decisions/tiers.rs: il vocabolario vive li'. */
const TIERS: PerformanceTier[] = ["light", "medium", "high", "heavy", "frontier"];

/** Opzioni della tendina di curatela: "auto" (nessun override) piu' la scala. */
const TIER_OPTIONS = [
  { value: "", label: "auto" },
  ...TIERS.map((t) => ({ value: t, label: t })),
];

/** Etichetta della PROVENIENZA del tier. E' l'informazione che dice se fidarsi
 *  del valore accanto: un tier senza fonte e' un fossile, non una decisione. */
const FONTE_LABEL: Record<string, string> = {
  synced: "indice",
  measured: "misurato",
  manual: "curato",
};

interface ProviderModelsSectionProps {
  /** Nome del provider (es. "perplexity"): filtra i modelli del catalog. */
  provider: string;
  /** Conteggio iniziale dei modelli abilitati (dai modelli gia' noti al parent),
   *  mostrato nel badge prima di espandere. Aggiornato al caricamento completo. */
  initialEnabledCount: number;
}

/**
 * Sezione espandibile "Modelli" dentro la card di un provider. Al primo
 * espandere carica i modelli del catalog INCLUSI i disabilitati
 * (`getProviderModelsAdmin`) e permette di abilitarli/disabilitarli
 * singolarmente. E' il controllo che rende usabile un provider onboardato
 * opt-in (i suoi modelli sono seedati is_enabled=false).
 */
export function ProviderModelsSection({
  provider,
  initialEnabledCount,
}: ProviderModelsSectionProps) {
  const tc = useThemeColors();
  const [open, setOpen] = useState(false);
  const [models, setModels] = useState<ModelCatalogEntry[] | null>(null);
  const [status, setStatus] = useState<"idle" | "loading" | "error">("idle");
  const [busy, setBusy] = useState<string | null>(null);
  const [toggleError, setToggleError] = useState<{ model: string; msg: string } | null>(null);

  // Conteggio mostrato: DERIVATO, mai uno useState inizializzato da una prop
  // async (che verrebbe letta solo al mount e ignorerebbe gli aggiornamenti).
  // Prima di espandere usa il conteggio del parent (prop reattiva); dopo il
  // caricamento usa la lista locale, cosi' l'aggiornamento del toggle si riflette.
  const displayCount =
    models === null ? initialEnabledCount : models.filter((m) => m.isEnabled).length;

  async function toggleOpen() {
    const next = !open;
    setOpen(next);
    if (next && models === null) {
      setStatus("loading");
      try {
        const d = await getProviderModelsAdmin(provider);
        setModels(d.models);
        setStatus("idle");
      } catch {
        setStatus("error");
      }
    }
  }

  function handleToggle(model: string, next: boolean) {
    setBusy(model);
    setToggleError(null);
    void setModelEnabled(provider, model, next)
      .then(() => {
        setModels((ms) =>
          ms ? ms.map((m) => (m.model === model ? { ...m, isEnabled: next } : m)) : ms,
        );
      })
      .catch((e) => {
        setToggleError({
          model,
          msg: e instanceof Error ? e.message : "aggiornamento fallito",
        });
      })
      .finally(() => setBusy(null));
  }

  /** Curatela del tier. `null` = rimuovi l'override: il valore resta, ma indice
   *  e batteria tornano a poterlo correggere. */
  function handleTier(model: string, tier: PerformanceTier | null) {
    setBusy(model);
    setToggleError(null);
    void setModelTier(provider, model, tier)
      .then(() => {
        setModels((ms) =>
          ms
            ? ms.map((m) =>
                m.model === model
                  ? {
                      ...m,
                      // Con l'override rimosso il tier NON si azzera: resta il
                      // valore corrente, con la fonte tornata ignota.
                      performanceTier: tier ?? m.performanceTier,
                      tierSource: tier ? "manual" : null,
                    }
                  : m,
              )
            : ms,
        );
      })
      .catch((e) => {
        setToggleError({
          model,
          msg: e instanceof Error ? e.message : "curatela fallita",
        });
      })
      .finally(() => setBusy(null));
  }

  return (
    <div style={{ marginTop: 8, borderTop: `1px solid ${tc.border}`, paddingTop: 8 }}>
      <button
        onClick={() => void toggleOpen()}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "2px 0",
          border: "none",
          background: "transparent",
          color: tc.textSecondary,
          fontSize: 12,
          fontWeight: 600,
          cursor: "pointer",
          fontFamily: "inherit",
        }}
      >
        <span style={{ transition: "transform 0.15s", transform: open ? "rotate(90deg)" : "none" }}>
          &#9656;
        </span>
        Modelli
        <span
          style={{
            padding: "1px 7px",
            borderRadius: 10,
            background: displayCount > 0 ? `${tc.success}22` : tc.bgInput,
            color: displayCount > 0 ? tc.success : tc.textMuted,
            fontSize: 11,
          }}
        >
          {displayCount} attivi
        </span>
      </button>

      {open && (
        <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 4 }}>
          {toggleError && (
            <div style={{ fontSize: 11, color: tc.error, padding: "2px 8px" }}>
              Errore su {toggleError.model}: {toggleError.msg}
            </div>
          )}
          {status === "loading" && (
            <div style={{ fontSize: 12, color: tc.textMuted }}>Caricamento modelli...</div>
          )}
          {status === "error" && (
            <div style={{ fontSize: 12, color: tc.error }}>
              Errore: impossibile caricare i modelli del catalog per {provider}.
            </div>
          )}
          {status === "idle" && models && models.length === 0 && (
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Nessun modello nel catalog per {provider}. Esegui la sincronizzazione
              del catalogo o aggiungi i modelli via migrazione.
            </div>
          )}
          {status === "idle" &&
            models &&
            models.map((m) => (
              <div
                key={m.model}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                  padding: "6px 8px",
                  borderRadius: 6,
                  background: tc.bgInput,
                  opacity: m.isEnabled ? 1 : 0.6,
                }}
              >
                <div style={{ minWidth: 0, display: "flex", flexDirection: "column", gap: 1 }}>
                  <span
                    style={{
                      fontSize: 12,
                      fontWeight: 600,
                      color: tc.text,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {m.displayName || m.model}
                  </span>
                  <span style={{ fontSize: 10, color: tc.textMuted }}>
                    {m.model}
                    {Array.isArray(m.capabilities) && m.capabilities.length > 0
                      ? ` · ${m.capabilities.join(", ")}`
                      : ""}
                  </span>
                  <span
                    style={{
                      fontSize: 10,
                      color: tc.textMuted,
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      flexWrap: "wrap",
                    }}
                  >
                    {/* La FONTE accanto al tier: un tier senza fonte e' un
                        fossile (l'euristica sul nome), non una decisione — ed e'
                        l'informazione che dice se fidarsi del valore. */}
                    <span
                      title={
                        m.tierSource
                          ? `tier ${m.performanceTier}: ${FONTE_LABEL[m.tierSource]}`
                          : "tier di provenienza ignota: nessuna fonte si e' espressa (indice o batteria possono correggerlo)"
                      }
                      style={{
                        padding: "0 4px",
                        borderRadius: 3,
                        border: `1px solid ${tc.border}`,
                        color: m.tierSource === "manual" ? tc.text : tc.textMuted,
                        fontWeight: m.tierSource === "manual" ? 600 : 400,
                      }}
                    >
                      {m.performanceTier ?? "tier ignoto"}
                      {m.tierSource ? ` · ${FONTE_LABEL[m.tierSource]}` : " · fonte ignota"}
                    </span>
                    {/* L'indice esterno: il numero su cui si fonda il tier
                        `synced`. Assente per il 63% del parco (OpenRouter non
                        lista quei nomi): li' decide solo la batteria. */}
                    <span
                      title={
                        m.agenticIndex === null
                          ? "il servizio di classificazione esterno non copre questo modello: il tier puo' venire solo dalla batteria"
                          : "agentic_index (Artificial Analysis): la capacita' MISURATA su harness con tool"
                      }
                    >
                      idx {m.agenticIndex ?? "—"}
                    </span>
                    {m.qualificationState && m.qualificationState !== "qualified" && (
                      <span
                        title="La batteria non ha (ancora) qualificato questo modello: col gate acceso resta fuori dal routing agentico"
                        style={{ color: tc.error }}
                      >
                        {m.qualificationState}
                      </span>
                    )}
                  </span>
                </div>
                <AutoWidthSelect
                  disabled={busy === m.model}
                  value={m.tierSource === "manual" ? (m.performanceTier ?? "") : ""}
                  onChange={(value) =>
                    handleTier(m.model, (value || null) as PerformanceTier | null)
                  }
                  title="Curatela del tier: vince su indice e batteria. Vuoto = nessun override (decidono le fonti automatiche)."
                  options={TIER_OPTIONS}
                  style={{
                    fontSize: 10,
                    padding: "2px 4px",
                    borderRadius: 4,
                    border: `1px solid ${tc.border}`,
                    background: tc.bgCard,
                    color: tc.text,
                    cursor: busy === m.model ? "not-allowed" : "pointer",
                    flexShrink: 0,
                  }}
                />
                <button
                  disabled={busy === m.model}
                  onClick={() => handleToggle(m.model, !m.isEnabled)}
                  title={m.isEnabled ? "Disabilita modello" : "Abilita modello"}
                  style={{
                    width: 38,
                    height: 20,
                    borderRadius: 10,
                    border: "none",
                    background:
                      busy === m.model ? tc.bgCard : m.isEnabled ? tc.success : tc.textMuted,
                    cursor: busy === m.model ? "not-allowed" : "pointer",
                    position: "relative",
                    flexShrink: 0,
                    outline: `1px solid ${m.isEnabled ? `${tc.success}60` : tc.border}`,
                    opacity: busy === m.model ? 0.6 : 1,
                  }}
                >
                  <span
                    style={{
                      position: "absolute",
                      top: 2,
                      left: m.isEnabled ? 19 : 2,
                      width: 16,
                      height: 16,
                      borderRadius: "50%",
                      background: "#fff",
                      transition: "left 0.2s",
                      boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
                    }}
                  />
                </button>
              </div>
            ))}
        </div>
      )}
    </div>
  );
}
