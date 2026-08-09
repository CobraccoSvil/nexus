"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";
import {
  usageBarView,
  rapportoBarra,
  costoLeggibile,
  NON_MISURATO,
  type SessionUsageState,
} from "./token-usage-bar-logic";

export interface TokenUsageBarProps {
  sessionId?: string;
  /** Contabilita' della conversazione dal ledger, con l'ignoto come variante.
   *  Prima erano due `number` sciolti, ed e' il motivo per cui il contatore ha
   *  potuto mostrare per un intero run i token di una chiamata accanto al costo
   *  della sessione: due numeri nudi non dicono a quale insieme appartengono, e
   *  nessun tipo impediva di scriverli da fonti diverse. */
  usage: SessionUsageState;
  budgetUsd?: number;
  /** Context window in token del modello corrente. Se presente, attiva l'indicatore di riempimento. */
  contextWindow?: number | null;
  /** Token di input usati nell'ultimo turno (stima del riempimento context window). */
  lastInputTokens?: number | null;
  /** Modello corrente, mostrato nel tooltip dettagliato. */
  modelLabel?: string | null;
}

export function TokenUsageBar({
  usage,
  budgetUsd,
  contextWindow,
  lastInputTokens,
  modelLabel,
}: TokenUsageBarProps) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const [expanded, setExpanded] = useState(false);

  const view = usageBarView(usage);
  if (!view.visibile) return null;

  const noto = usage.stato === "noto" ? usage : null;
  const rapporto = rapportoBarra({
    misurato: view.misurato,
    totalCostUsd: noto?.sessione.totalCostUsd ?? 0,
    budgetUsd,
    contextWindow,
    lastInputTokens,
  });

  const hasBudget = rapporto?.base === "budget";
  const hasContext =
    contextWindow != null &&
    contextWindow > 0 &&
    lastInputTokens != null &&
    lastInputTokens > 0;

  let barColor = tc.textMuted;
  if (rapporto != null) {
    if (rapporto.valore < 0.5) barColor = tc.success;
    else if (rapporto.valore < 0.8) barColor = tc.warning;
    else barColor = tc.error;
  }
  // Un dato non misurato non prende il colore di una soglia: sarebbe un giudizio
  // su un numero che non c'e'.
  if (!view.misurato) barColor = tc.warning;

  const fillPct = rapporto != null ? Math.min(rapporto.valore * 100, 100) : null;

  return (
    <div style={{ position: "relative" }}>
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        title={
          hasBudget
            ? `${view.titolo} Budget: $${budgetUsd!.toFixed(2)}`
            : view.titolo
        }
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          height: 24,
          padding: "0 8px",
          borderRadius: 6,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          cursor: "pointer",
          color: rapporto != null || !view.misurato ? barColor : tc.textMuted,
          fontSize: 11,
          fontFamily: "inherit",
          whiteSpace: "nowrap",
          width: "100%",
          overflow: "hidden",
          position: "relative",
        }}
      >
        {/* progress fill */}
        {fillPct != null && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              width: `${fillPct}%`,
              background: barColor,
              opacity: 0.12,
              borderRadius: 6,
              pointerEvents: "none",
            }}
          />
        )}
        <span style={{ position: "relative", zIndex: 1 }}>
          {view.tokensLabel} &bull; {view.costLabel}
          {rapporto != null && (
            <span style={{ marginLeft: 4, opacity: 0.8 }}>
              ({Math.round(rapporto.valore * 100)}% {hasBudget ? "budget" : "ctx"})
            </span>
          )}
        </span>
        <span
          style={{
            marginLeft: "auto",
            fontSize: 9,
            opacity: 0.5,
            position: "relative",
            zIndex: 1,
          }}
        >
          {expanded ? "▲" : "▼"}
        </span>
      </button>

      {expanded && (
        <div
          style={{
            position: "absolute",
            bottom: "calc(100% + 4px)",
            left: 0,
            right: 0,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            padding: "8px 10px",
            fontSize: 11,
            color: tc.textMuted,
            zIndex: 20,
            boxShadow: "0 4px 16px rgba(0,0,0,0.25)",
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: 6, color: tc.textMuted }}>
            {t("chat.dettaglioSessione")}
          </div>
          {!view.misurato && (
            <div style={{ marginBottom: 6, color: tc.warning, lineHeight: 1.4 }}>
              {usage.stato === "non_disponibile"
                ? `Contabilita' non leggibile (${usage.motivo}). Nessun numero mostrato: un valore vecchio sarebbe indistinguibile da uno fresco.`
                : "Contabilita' non ancora letta."}
            </div>
          )}
          <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 2 }}>
            <span>{t("chat.tokenTotali")}</span>
            <span style={{ color: barColor }}>
              {noto ? noto.sessione.totalTokens.toLocaleString("it-IT") : NON_MISURATO}
            </span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 2 }}>
            <span>{t("chat.costoTotale")}</span>
            <span style={{ color: barColor }}>
              {noto ? costoLeggibile(noto.sessione.totalCostUsd) : NON_MISURATO}
            </span>
          </div>
          {/* Il run corrente e' un PERIMETRO DIVERSO, e lo dice. Sulla sessione
              misurata l'08/08/2026 valevano $2,6024 e $0,1272: senza questa riga
              chi deve decidere se un run e' costato troppo legge il totale della
              conversazione e conclude il contrario. */}
          {view.runLabel && (
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 8,
                marginTop: 4,
                paddingTop: 4,
                borderTop: `1px solid ${tc.border}`,
              }}
            >
              <span>{t("chat.diCuiRunCorrente")}</span>
              <span style={{ color: barColor, textAlign: "right" }}>{view.runLabel}</span>
            </div>
          )}
          {hasBudget && (
            <div style={{ display: "flex", justifyContent: "space-between" }}>
              <span>{t("chat.budget")}</span>
              <span>${budgetUsd!.toFixed(2)}</span>
            </div>
          )}
          {hasContext && (
            <>
              <div style={{ display: "flex", justifyContent: "space-between", marginTop: 4 }}>
                <span>{t("chat.ultimoInput")}</span>
                <span style={{ color: barColor }}>
                  {lastInputTokens!.toLocaleString()} token
                </span>
              </div>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span>Context window{modelLabel ? ` (${modelLabel})` : ""}</span>
                <span>{contextWindow!.toLocaleString()} token</span>
              </div>
              {rapporto != null && rapporto.valore >= 0.7 && !hasBudget && (
                <div
                  style={{
                    marginTop: 6,
                    paddingTop: 6,
                    borderTop: `1px solid ${tc.border}`,
                    color: rapporto.valore >= 0.8 ? tc.error : tc.warning,
                    fontSize: 10,
                    lineHeight: 1.4,
                  }}
                >
                  {rapporto.valore >= 0.8
                    ? "Context quasi pieno: compatta la chat (icona ⌁) per evitare perdita di informazioni."
                    : "Context sopra il 70%: valuta di compattare la chat a breve."}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
