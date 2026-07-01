"use client";

import type { AgentRunInfo, AgentStep } from "../../lib/api-client";
import { toolLabel } from "./tool-labels";

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
}: {
  tc: Record<string, string>;
  isAgentStuck: boolean;
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
}) {
  return (
    <div
      style={{
        margin: "6px 0 0",
        borderRadius: 8,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        display: "flex",
        alignItems: "center",
        gap: 8,
        flexShrink: 0,
        boxShadow: "0 2px 8px rgba(0,0,0,0.10)",
      }}
      aria-live="polite"
    >
      {/* Riga principale */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "7px 10px" }}>
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
        <strong style={{ color: isAgentStuck ? "#f97316" : tc.text, fontSize: 12 }}>
          {secondsSinceLastStep > 120 ? "⚠ AI in elaborazione" : isAgentStuck ? "⚠ Agente in attesa" : busyLabel}
        </strong>
        {isAgentRunning && runningAgentStep ? (
          <span style={{ color: tc.textMuted, fontSize: 11 }}>
            step {runningAgentStep.stepIndex + 1} • {toolLabel(runningAgentStep.toolName)}
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
          <span style={{
            fontSize: 10,
            color: secondsSinceLastStep > 120 ? "#f97316" : tc.textMuted,
            fontVariantNumeric: "tabular-nums",
            marginLeft: 4,
          }}>
            {secondsSinceLastStep < 60
              ? `${secondsSinceLastStep}s`
              : `${Math.floor(secondsSinceLastStep / 60)}m ${secondsSinceLastStep % 60}s`}
          </span>
        )}
        {secondsSinceLastStep > 120 && agentRun?.runId && (
          <button
            type="button"
            onClick={() => onCancelRun(agentRun.runId)}
            style={{
              fontSize: 10, padding: "2px 8px", borderRadius: 4,
              border: "1px solid #f9731680", background: "#f9731618",
              color: "#f97316", cursor: "pointer", fontWeight: 600,
            }}
          >
            Forza stop
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
      </div>
      {/* Dettagli espandibili */}
      {isAgentRunning && agentStatusExpanded && (
        <div style={{
          borderTop: `1px solid ${tc.border}`,
          padding: "6px 10px 8px",
          display: "flex", flexDirection: "column", gap: 4,
        }}>
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
                  {step.stepIndex + 1}. {toolLabel(step.toolName)} —{" "}
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
