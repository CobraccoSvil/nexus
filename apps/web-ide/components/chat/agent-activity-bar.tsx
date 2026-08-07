"use client";

import type { ReactNode } from "react";
import type { AgentRunInfo, AgentStep } from "../../lib/api-client";
import { stepLabel } from "./tool-labels";
import {
  activityStatusView,
  formatDuration,
  interruptButtonView,
} from "./interrupt-button-logic";
import { useI18n } from "../../lib/i18n";

/**
 * Barra di stato "AI in esecuzione": riassume lo stato del run agente in corso
 * (o di un'operazione sui messaggi) con un timer monotono, l'attivita' corrente
 * e una sezione espandibile di dettagli (step completati, comando, output,
 * timeline). Tutti i valori derivati sono calcolati nell'orchestratore
 * (chat-panel) e passati via props per non duplicare la logica dei timer SSE.
 */
export function AgentActivityBar({
  tc,
  isAgentStuck,
  runElapsedSeconds,
  secondsSinceLastStep,
  busyLabel,
  isAgentRunning,
  runningAgentStep,
  lastMetaStep,
  agentRun,
  onCancelRun,
  agentStatusExpanded,
  onToggleExpanded,
  completedSteps,
  runningSteps,
  failedSteps,
  runningCommand,
  latestOutputSnippet,
  latestStepWithOutputResult,
  timelineSteps,
  trailing,
}: {
  tc: Record<string, string>;
  isAgentStuck: boolean;
  /**
   * Secondi dall'AVVIO del run: e' il timer che la barra mostra e la grandezza
   * su cui scattano l'evidenza arancione e il pulsante di interruzione. Si
   * chiamava `secondsSinceLastStep` pur misurando l'avvio run, ed e' cosi' che
   * la comparsa del pulsante veniva letta come "agente bloccato".
   */
  runElapsedSeconds: number;
  /** Secondi dall'ultimo step o meta-step: l'inattivita' vera, per i tooltip. */
  secondsSinceLastStep: number;
  busyLabel: string;
  isAgentRunning: boolean;
  runningAgentStep: AgentStep | null;
  lastMetaStep: { title: string } | null;
  agentRun: AgentRunInfo | null;
  onCancelRun: (runId: string) => void;
  agentStatusExpanded: boolean;
  onToggleExpanded: () => void;
  completedSteps: number;
  runningSteps: number;
  failedSteps: number;
  runningCommand: string | null;
  latestOutputSnippet: string | undefined;
  latestStepWithOutputResult: string | undefined;
  timelineSteps: AgentStep[];
  /** Contenuto agganciato in coda alla riga di stato (es. centro notifiche del
   *  run). Sta QUI e non in una riga propria: la barra esiste gia' per l'intera
   *  durata del run, quindi ospitarlo non costa altezza. */
  trailing?: ReactNode;
}) {
  const { t } = useI18n();
  const interrupt = interruptButtonView({
    runElapsedSeconds,
    secondsSinceLastStep,
    isAgentStuck,
  });
  const status = activityStatusView({
    runElapsedSeconds,
    secondsSinceLastStep,
    isAgentStuck,
    busyLabel,
  });
  return (
    <div
      style={{
        margin: "6px 0 0",
        borderRadius: 8,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        // I due blocchi (riga di stato + dettagli) si IMPILANO. Era `flex` in
        // direzione riga di default: stavano quindi affiancati, la riga di stato
        // si dimensionava sul proprio contenuto e sfondava la card, spingendo
        // fuori dal bordo lo slot in coda (il centro notifiche del run). Che i
        // dettagli vadano sotto lo dice il loro stesso `borderTop`.
        display: "flex",
        flexDirection: "column",
        alignItems: "stretch",
        flexShrink: 0,
        // Nessun gap: i dettagli sono gia' separati dal proprio borderTop, e uno
        // spazio qui aprirebbe una fessura nel bordo della card.
        overflow: "hidden",
        boxShadow: "0 2px 8px rgba(0,0,0,0.10)",
      }}
      aria-live="polite"
    >
      {/* Riga principale. `minWidth: 0` e' cio' che permette ai figli con
          ellipsis (l'attivita' corrente) di restringersi davvero: senza, un flex
          item non scende sotto il proprio contenuto minimo e la riga cresce oltre
          la card invece di troncare il testo. */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "7px 10px",
          minWidth: 0,
        }}
      >
        <span
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "#22c55e",
            boxShadow: "0 0 0 2px #22c55e33",
            animation: "pulse 1s ease-in-out infinite",
            flexShrink: 0,
          }}
        />
        <strong
          title={status.title}
          style={{ color: status.warn ? "#f97316" : tc.text, fontSize: 12 }}
        >
          {status.text}
        </strong>
        {isAgentRunning && runningAgentStep ? (
          <span style={{ color: tc.textMuted, fontSize: 11 }}>
            {stepLabel(runningAgentStep)}
          </span>
        ) : isAgentRunning && lastMetaStep ? (
          // Attivita' corrente live dal flusso meta_step (es. "tool
          // edit_file — vite.config.ts"): aggiornata in tempo reale anche
          // quando agentSteps non e' ancora popolato.
          <span
            style={{
              color: tc.textMuted,
              fontSize: 11,
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
              maxWidth: 320,
            }}
            title={lastMetaStep.title}
          >
            {lastMetaStep.title}
          </span>
        ) : null}
        {isAgentRunning && (
          <span
            title={t("chat.tempoTrascorsoDallAvvio")}
            style={{
              fontSize: 10,
              // Neutro: il cronometro dice il TEMPO, non un giudizio. Sopra i
              // due minuti diventava arancione, cioe' segnalava come anomalia un
              // run lungo che sta procedendo — e l'unica anomalia vera (agente
              // FERMO) ha gia' il suo avviso nella riga di stato, col tempo di
              // inattivita' dentro. Due allarmi di cui uno falso non fanno
              // guardare meglio: fanno smettere di guardare.
              color: tc.textMuted,
              fontVariantNumeric: "tabular-nums",
              marginLeft: 4,
            }}
          >
            {formatDuration(runElapsedSeconds)}
          </span>
        )}
        {interrupt.visible && agentRun?.runId && (
          <button
            type="button"
            onClick={() => onCancelRun(agentRun.runId)}
            title={interrupt.title}
            style={{
              fontSize: 10, padding: "2px 8px", borderRadius: 4,
              border: "1px solid #f9731680", background: "#f9731618",
              color: "#f97316", cursor: "pointer", fontWeight: 600,
            }}
          >
            {interrupt.label}
          </button>
        )}
        {isAgentRunning && (
          <button
            type="button"
            onClick={onToggleExpanded}
            title={agentStatusExpanded ? "Comprimi dettagli" : "Espandi dettagli"}
            style={{
              marginLeft: "auto", border: `1px solid ${tc.border}`,
              background: "transparent", color: tc.text, borderRadius: 6,
              width: 22, height: 22, display: "inline-flex", alignItems: "center",
              justifyContent: "center", cursor: "pointer", fontSize: 11,
            }}
          >
            {agentStatusExpanded ? "▾" : "▸"}
          </button>
        )}
        {/* Slot in coda: ospita il centro notifiche del run. Se il toggle sopra
            e' presente, il suo marginLeft:auto ha gia' spinto il gruppo a destra
            e qui basta un piccolo distacco; altrimenti ci si spinge da soli. */}
        {trailing ? (
          <span
            style={{
              marginLeft: isAgentRunning ? 4 : "auto",
              display: "inline-flex",
              alignItems: "center",
              flex: "0 0 auto",
            }}
          >
            {trailing}
          </span>
        ) : null}
      </div>
      {/* Dettagli espandibili */}
      {isAgentRunning && agentStatusExpanded && (
        <div style={{
          borderTop: `1px solid ${tc.border}`,
          padding: "6px 10px 8px",
          display: "flex", flexDirection: "column", gap: 4,
        }}>
          {/* Metriche del run in corso. Stavano in una card separata del pannello
              step (`agent-steps-panel`), adiacente a questa e con le STESSE cifre:
              qui appartengono, perche' questa e' la card del run attivo. Il
              pannello le mostra ora solo per i run gia' conclusi. */}
          {(agentRun?.usage?.totalTokens || agentRun?.totalCostUsd) && (
            <div style={{ color: tc.textMuted, fontSize: 11, display: "flex", gap: 10, flexWrap: "wrap" }}>
              {agentRun?.usage?.totalTokens ? (
                <span>
                  Token totali:{" "}
                  <strong style={{ fontFamily: "var(--font-mono)", color: tc.text }}>
                    {agentRun.usage.totalTokens.toLocaleString()}
                  </strong>
                </span>
              ) : null}
              {agentRun?.totalCostUsd ? (
                <span>
                  Costo:{" "}
                  <strong style={{ fontFamily: "var(--font-mono)", color: tc.text }}>
                    ${agentRun.totalCostUsd.toFixed(6)}
                  </strong>
                </span>
              ) : null}
              {agentRun?.createdAt ? (
                <span>
                  Inizio:{" "}
                  <strong style={{ fontFamily: "var(--font-mono)", color: tc.textMuted }}>
                    {new Date(agentRun.createdAt).toLocaleTimeString()}
                  </strong>
                </span>
              ) : null}
            </div>
          )}
          <div style={{ color: tc.textMuted, fontSize: 11 }}>
            Step completati: {completedSteps}
            {runningSteps > 0 ? ` • in corso: ${runningSteps}` : ""}
            {failedSteps > 0 ? ` • falliti: ${failedSteps}` : ""}
          </div>
          {runningCommand && (
            <div style={{
              fontFamily: "var(--font-mono)",
              fontSize: 11, color: tc.textSecondary,
              whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
            }} title={runningCommand}>
              cmd: {runningCommand}
            </div>
          )}
          {latestOutputSnippet && (
            <div style={{
              fontSize: 11, color: tc.textMuted,
              whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
            }} title={latestStepWithOutputResult}>
              output: {latestOutputSnippet}
            </div>
          )}
          {timelineSteps.length > 0 && (
            <div style={{ marginTop: 2, paddingTop: 4, borderTop: `1px dashed ${tc.border}`, display: "flex", flexDirection: "column", gap: 1 }}>
              {timelineSteps.map((step) => (
                <div
                  key={`tl-${step.stepIndex}`}
                  style={{
                    color: step.status === "failed" ? tc.error : tc.textSecondary,
                    fontSize: 11, fontFamily: "var(--font-mono)",
                    whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis",
                  }}
                >
                  {stepLabel(step)} —{" "}
                  {step.status === "completed" ? "ok" : step.status === "running" ? "in corso" : step.status === "failed" ? "errore" : step.status}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
