"use client";

import React, { useState } from "react";
import type { AgentRunInfo, AgentStep } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { MarkdownBlock } from "./markdown-renderer";

type ThemeColors = ReturnType<typeof useThemeColors>;

export interface AgentStepsPanelProps {
  agentRun: AgentRunInfo;
  agentSteps: AgentStep[];
  tc: ThemeColors;
  t: (key: string) => string;
  onConfirm: (runId: string, approved: boolean) => void;
  // Opzionale: mappa di run paralleli (se presente, mostra tabs)
  agentRuns?: Map<string, AgentRunInfo>;
  agentStepsMap?: Map<string, AgentStep[]>;
  streamingToken?: string;
}

function SingleRunPanel({
  run,
  steps,
  tc,
  t,
  onConfirm,
  label,
  streamingToken,
}: {
  run: AgentRunInfo;
  steps: AgentStep[];
  tc: ThemeColors;
  t: (key: string) => string;
  onConfirm: (runId: string, approved: boolean) => void;
  label?: string;
  streamingToken?: string;
}) {
  const [expandedMetrics, setExpandedMetrics] = React.useState(false);
  const [expandedStepIndex, setExpandedStepIndex] = React.useState<number | null>(null);
  const displayLabel = label !== undefined ? label : `${run.provider}/${run.model}`;

  // Calcola metriche totali dagli step
  const calculateMetrics = () => {
    let totalTokens = 0;
    let totalCost = 0;
    let maxLatency = 0;
    let cacheHitCount = 0;

    steps.forEach((step) => {
      if (step.usage?.totalTokens) totalTokens += step.usage.totalTokens;
      if (step.costUsd) totalCost += step.costUsd;
      if (step.latencyMs) maxLatency = Math.max(maxLatency, step.latencyMs);
      if (step.usage?.cacheReadTokens) cacheHitCount += step.usage.cacheReadTokens;
    });

    // Usa metriche del run se disponibili, altrimenti quelle calcolate
    const totalTokensDisplay = run.usage?.totalTokens ?? totalTokens;
    const totalCostDisplay = run.totalCostUsd ?? totalCost;
    const cacheHitRate = run.cacheHitRate ?? (totalTokensDisplay > 0 ? (cacheHitCount / totalTokensDisplay) * 100 : 0);

    return {
      totalTokens: totalTokensDisplay,
      totalCost: totalCostDisplay,
      maxLatency,
      cacheHitRate,
      cacheReadTokens: run.usage?.cacheReadTokens ?? cacheHitCount,
    };
  };

  const metrics = calculateMetrics();

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {displayLabel.trim().length > 0 && (
        <div style={{ fontWeight: 600, fontSize: 11, color: tc.textMuted }}>
          {displayLabel}
        </div>
      )}

      {/* Sezione riepilogo metriche estese */}
      {(metrics.totalTokens > 0 || metrics.totalCost > 0) && (
        <div>
          <div
            onClick={() => setExpandedMetrics(!expandedMetrics)}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontSize: 11,
              fontWeight: 500,
              color: tc.textSecondary,
              cursor: "pointer",
              padding: "4px 6px",
              borderRadius: 4,
              background: `${tc.border}20`,
              transition: "background 0.2s",
              userSelect: "none",
            }}
            onMouseEnter={(e) => {
              if (e.currentTarget instanceof HTMLElement) {
                e.currentTarget.style.background = `${tc.border}40`;
              }
            }}
            onMouseLeave={(e) => {
              if (e.currentTarget instanceof HTMLElement) {
                e.currentTarget.style.background = `${tc.border}20`;
              }
            }}
          >
            <span style={{ fontSize: 10, opacity: 0.7 }}>
              {expandedMetrics ? "▼" : "▶"}
            </span>
            <span>Metriche estese</span>
          </div>

          {expandedMetrics && (
            <div
              style={{
                marginTop: 6,
                padding: "8px 8px",
                borderRadius: 4,
                background: `${tc.bgInput}80`,
                border: `1px solid ${tc.border}40`,
                display: "grid",
                gridTemplateColumns: "1fr 1fr",
                gap: 8,
                fontSize: 11,
              }}
            >
              {/* Token totali */}
              {metrics.totalTokens > 0 && (
                <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                  <span style={{ opacity: 0.6 }}>Tokens:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 600, color: tc.text }}>
                    {metrics.totalTokens.toLocaleString()}
                  </span>
                </div>
              )}

              {/* Costo totale */}
              {metrics.totalCost > 0 && (
                <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                  <span style={{ opacity: 0.6 }}>Costo:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 600, color: tc.text }}>
                    ${metrics.totalCost.toFixed(6)}
                  </span>
                </div>
              )}

              {/* Cache hit rate */}
              {metrics.cacheHitRate > 0 && (
                <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                  <span style={{ opacity: 0.6 }}>Cache hit:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 600, color: "#22c55e" }}>
                    {metrics.cacheHitRate.toFixed(1)}%
                  </span>
                </div>
              )}

              {/* Latency massima */}
              {metrics.maxLatency > 0 && (
                <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                  <span style={{ opacity: 0.6 }}>Latency:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 600, color: tc.text }}>
                    {metrics.maxLatency}ms
                  </span>
                </div>
              )}

              {/* Timestamp */}
              {run.createdAt && (
                <div style={{ display: "flex", alignItems: "center", gap: 4, gridColumn: "1 / -1" }}>
                  <span style={{ opacity: 0.6 }}>Inizio:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 500, color: tc.textMuted, fontSize: 10 }}>
                    {new Date(run.createdAt).toLocaleTimeString()}
                  </span>
                </div>
              )}

              {/* Timestamp fine */}
              {run.completedAt && (
                <div style={{ display: "flex", alignItems: "center", gap: 4, gridColumn: "1 / -1" }}>
                  <span style={{ opacity: 0.6 }}>Fine:</span>
                  <span style={{ fontFamily: "monospace", fontWeight: 500, color: tc.textMuted, fontSize: 10 }}>
                    {new Date(run.completedAt).toLocaleTimeString()}
                  </span>
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {steps.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 3 }}>
          {/* Raggruppa step consecutivi con stesso toolName e stesso status */}
          {(() => {
            type GroupedStep = {
              step: AgentStep;
              count: number;
              firstIndex: number;
              lastIndex: number;
            };
            const groups: GroupedStep[] = [];
            for (const step of steps) {
              const last = groups[groups.length - 1];
              if (
                last &&
                last.step.toolName === step.toolName &&
                last.step.status === step.status &&
                step.toolName !== "supervisor_check"
              ) {
                last.count += 1;
                last.lastIndex = step.stepIndex;
                // Aggiorna lo step del gruppo con l'ultimo (per status running)
                last.step = step;
              } else {
                groups.push({ step, count: 1, firstIndex: step.stepIndex, lastIndex: step.stepIndex });
              }
            }

            return groups.map(({ step, count, firstIndex, lastIndex }) => {
              const hasExtendedMetrics = step.usage || step.costUsd || step.latencyMs || step.temperature !== undefined;

              return (
                <div
                  key={`${step.stepIndex}-${count}`}
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 4,
                  }}
                >
                  <div
                    onClick={() => hasExtendedMetrics && setExpandedStepIndex(expandedStepIndex === step.stepIndex ? null : step.stepIndex)}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      fontSize: 12,
                      color: step.status === "failed" ? tc.error : tc.textSecondary,
                      cursor: hasExtendedMetrics ? "pointer" : "default",
                      padding: hasExtendedMetrics ? "2px 4px" : "0",
                      borderRadius: 3,
                      transition: "background 0.15s",
                      background: expandedStepIndex === step.stepIndex ? `${tc.border}15` : "transparent",
                    }}
                    onMouseEnter={(e) => {
                      if (hasExtendedMetrics && e.currentTarget instanceof HTMLElement) {
                        e.currentTarget.style.background = `${tc.border}20`;
                      }
                    }}
                    onMouseLeave={(e) => {
                      if (hasExtendedMetrics && e.currentTarget instanceof HTMLElement) {
                        e.currentTarget.style.background = expandedStepIndex === step.stepIndex ? `${tc.border}15` : "transparent";
                      }
                    }}
                  >
                    {/* Numero di riga: range se raggruppati */}
                    <span style={{ minWidth: 28, textAlign: "right", opacity: 0.5, fontSize: 11, fontVariantNumeric: "tabular-nums" }}>
                      {count > 1 ? `${firstIndex + 1}–${lastIndex + 1}` : `${firstIndex + 1}.`}
                    </span>

                    {/* Icona expand se ha metriche */}
                    {hasExtendedMetrics && (
                      <span style={{ fontSize: 10, opacity: 0.6, minWidth: 12, textAlign: "center" }}>
                        {expandedStepIndex === step.stepIndex ? "▼" : "▶"}
                      </span>
                    )}

                    {step.toolName === "supervisor_check" ? (
                      <span style={{ color: "#8b5cf6", fontWeight: 600 }}>
                        👁 supervisor
                      </span>
                    ) : (
                      <span style={{ fontFamily: "monospace" }}>{step.toolName}</span>
                    )}

                    {step.status === "running" && <span style={{ opacity: 0.6 }}>...</span>}

                    {step.status === "completed" && step.toolName !== "supervisor_check" && (
                      <span style={{ color: "#22c55e" }}>✓</span>
                    )}
                    {step.status === "completed" && step.toolName === "supervisor_check" && step.toolResult && (
                      <span style={{
                        color: step.toolResult.startsWith("↩") ? "#f97316" :
                               step.toolResult.startsWith("⛔") ? tc.error : "#8b5cf6",
                        fontSize: 11,
                      }}>
                        {step.toolResult}
                      </span>
                    )}
                    {step.status === "failed" && <span style={{ color: tc.error }}>✗</span>}

                    {/* Badge contatore — visibile solo se ci sono più step raggruppati */}
                    {count > 1 && (
                      <span style={{
                        marginLeft: 2,
                        background: step.status === "failed" ? `${tc.error}22` : "#22c55e22",
                        color: step.status === "failed" ? tc.error : "#22c55e",
                        border: `1px solid ${step.status === "failed" ? tc.error : "#22c55e"}44`,
                        borderRadius: 10,
                        padding: "0px 6px",
                        fontSize: 10,
                        fontWeight: 700,
                        fontVariantNumeric: "tabular-nums",
                        lineHeight: "16px",
                      }}>
                        ×{count}
                      </span>
                    )}
                  </div>

                  {/* Dettagli estesi dello step */}
                  {expandedStepIndex === step.stepIndex && hasExtendedMetrics && (
                    <div
                      style={{
                        marginLeft: 20,
                        paddingLeft: 8,
                        paddingRight: 8,
                        paddingTop: 6,
                        paddingBottom: 6,
                        borderLeft: `2px solid ${tc.border}40`,
                        fontSize: 11,
                        color: tc.textSecondary,
                        display: "grid",
                        gridTemplateColumns: "1fr 1fr",
                        gap: 6,
                      }}
                    >
                      {/* Token consumati */}
                      {step.usage && (
                        <>
                          {step.usage.promptTokens !== undefined && (
                            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                              <span style={{ opacity: 0.7 }}>Input tokens:</span>
                              <span style={{ fontFamily: "monospace", fontWeight: 500 }}>
                                {step.usage.promptTokens.toLocaleString()}
                              </span>
                            </div>
                          )}
                          {step.usage.completionTokens !== undefined && (
                            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                              <span style={{ opacity: 0.7 }}>Output tokens:</span>
                              <span style={{ fontFamily: "monospace", fontWeight: 500 }}>
                                {step.usage.completionTokens.toLocaleString()}
                              </span>
                            </div>
                          )}
                          {step.usage.totalTokens !== undefined && (
                            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                              <span style={{ opacity: 0.7 }}>Totale token:</span>
                              <span style={{ fontFamily: "monospace", fontWeight: 600 }}>
                                {step.usage.totalTokens.toLocaleString()}
                              </span>
                            </div>
                          )}
                          {step.usage.cacheReadTokens !== undefined && step.usage.cacheReadTokens > 0 && (
                            <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                              <span style={{ opacity: 0.7 }}>Cache read:</span>
                              <span style={{ fontFamily: "monospace", fontWeight: 500, color: "#22c55e" }}>
                                {step.usage.cacheReadTokens.toLocaleString()}
                              </span>
                            </div>
                          )}
                        </>
                      )}

                      {/* Costo */}
                      {step.costUsd !== undefined && (
                        <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                          <span style={{ opacity: 0.7 }}>Costo:</span>
                          <span style={{ fontFamily: "monospace", fontWeight: 600 }}>
                            ${step.costUsd.toFixed(6)}
                          </span>
                        </div>
                      )}

                      {/* Latency */}
                      {step.latencyMs !== undefined && (
                        <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                          <span style={{ opacity: 0.7 }}>Latency:</span>
                          <span style={{ fontFamily: "monospace", fontWeight: 500 }}>
                            {step.latencyMs}ms
                          </span>
                        </div>
                      )}

                      {/* Temperature */}
                      {step.temperature !== undefined && (
                        <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                          <span style={{ opacity: 0.7 }}>Temperature:</span>
                          <span style={{ fontFamily: "monospace", fontWeight: 500 }}>
                            {step.temperature.toFixed(2)}
                          </span>
                        </div>
                      )}

                      {/* Top P */}
                      {step.topP !== undefined && (
                        <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                          <span style={{ opacity: 0.7 }}>Top P:</span>
                          <span style={{ fontFamily: "monospace", fontWeight: 500 }}>
                            {step.topP.toFixed(2)}
                          </span>
                        </div>
                      )}

                      {/* Timestamp */}
                      {step.createdAt && (
                        <div style={{ display: "flex", gap: 4, alignItems: "center", gridColumn: "1 / -1", fontSize: 10 }}>
                          <span style={{ opacity: 0.6 }}>Eseguito:</span>
                          <span style={{ fontFamily: "monospace", color: tc.textMuted }}>
                            {new Date(step.createdAt).toLocaleTimeString()}
                          </span>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            });
          })()}
        </div>
      )}

      {run.status === "awaiting_confirmation" && run.pendingActions.length > 0 && (
        <div>
          <div style={{ color: tc.text, marginBottom: 6, fontWeight: 500, fontSize: 12 }}>
            Azioni in attesa di conferma:
          </div>
          {run.pendingActions.map((action) => (
            <div
              key={action.index}
              style={{
                fontFamily: "monospace",
                background: `${tc.border}40`,
                borderRadius: 4,
                padding: "2px 6px",
                marginBottom: 4,
                color: tc.text,
                fontSize: 11,
              }}
            >
              {action.description}
            </div>
          ))}
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            <button
              onClick={() => onConfirm(run.runId, true)}
              style={{
                padding: "4px 14px",
                borderRadius: 6,
                border: "none",
                background: "#22c55e",
                color: "#fff",
                cursor: "pointer",
                fontWeight: 600,
                fontSize: 12,
              }}
            >
              Approva
            </button>
            <button
              onClick={() => onConfirm(run.runId, false)}
              style={{
                padding: "4px 14px",
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: "transparent",
                color: tc.error,
                cursor: "pointer",
                fontWeight: 600,
                fontSize: 12,
              }}
            >
              Annulla
            </button>
          </div>
        </div>
      )}

      {run.status === "running" && steps.length === 0 && !streamingToken && (
        <div style={{ color: tc.textMuted, fontStyle: "italic", fontSize: 12 }}>
          {t("chat.thinking")}
        </div>
      )}
      {run.status === "running" && streamingToken && (
        <div style={{ color: tc.text, fontSize: 13, wordBreak: "break-word" }}>
          <MarkdownBlock content={streamingToken} />
          <span
            style={{
              display: "inline-block",
              width: 2,
              height: "1em",
              background: tc.text,
              verticalAlign: "text-bottom",
              marginLeft: 1,
              animation: "nexus-blink 1s step-end infinite",
            }}
          />
        </div>
      )}

      {(run.status === "completed" || run.status === "failed") && (
        <div style={{
          fontSize: 11,
          color: run.status === "failed" ? tc.error : "#22c55e",
          fontWeight: 600,
        }}>
          {run.status === "completed" ? "✓ Completato" : "✗ Fallito"} — {steps.length} step
        </div>
      )}
    </div>
  );
}

export function AgentStepsPanel({
  agentRun,
  agentSteps,
  tc,
  t,
  onConfirm,
  agentRuns,
  agentStepsMap,
  streamingToken,
}: AgentStepsPanelProps) {
  const [activeTab, setActiveTab] = useState<string>(agentRun.runId);

  // Costruisce la lista dei run da mostrare
  const allRuns: Array<{ run: AgentRunInfo; steps: AgentStep[]; label: string }> = [];

  if (agentRuns && agentRuns.size > 0) {
    let index = 1;
    agentRuns.forEach((run, runId) => {
      const steps = agentStepsMap?.get(runId) ?? [];
      const isChild = run.runId !== agentRun.runId;
      allRuns.push({
        run,
        steps,
        label: isChild ? `Sub-agente ${index++}` : "Agente principale",
      });
    });
  } else {
    allRuns.push({ run: agentRun, steps: agentSteps, label: `${agentRun.provider}/${agentRun.model}` });
  }

  const isMulti = allRuns.length > 1;
  const activeRunData = isMulti
    ? allRuns.find((r) => r.run.runId === activeTab) ?? allRuns[0]
    : allRuns[0];

  return (
    <div
      style={{
        border: `1px solid ${tc.border}`,
        borderRadius: 10,
        background: tc.bgCard,
        padding: "10px 12px",
        alignSelf: "stretch",
        fontSize: 12,
      }}
    >
      {/* Header */}
      <div style={{ fontWeight: 600, marginBottom: isMulti ? 8 : 6, color: tc.text, fontSize: 12, display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        {isMulti
          ? `⚡ ${allRuns.length} agenti in parallelo`
          : "Agente in esecuzione"}
        {!isMulti && (
          <span style={{
            fontSize: 10,
            fontWeight: 500,
            padding: "1px 7px",
            borderRadius: 999,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: tc.textMuted,
            fontFamily: "monospace",
          }}>
            {agentRun.provider}/{agentRun.model}
          </span>
        )}
      </div>

      {/* Tabs — solo se ci sono più run */}
      {isMulti && (
        <div style={{ display: "flex", gap: 4, marginBottom: 10, flexWrap: "wrap" }}>
          {allRuns.map(({ run, steps, label }) => {
            const isActive = activeTab === run.runId;
            const completedSteps = steps.filter((s) => s.status === "completed").length;
            return (
              <button
                key={run.runId}
                onClick={() => setActiveTab(run.runId)}
                style={{
                  padding: "3px 10px",
                  borderRadius: 6,
                  border: `1px solid ${isActive ? tc.accent : tc.border}`,
                  background: isActive ? tc.accentBg : "transparent",
                  color: isActive ? tc.accent : tc.textMuted,
                  cursor: "pointer",
                  fontSize: 11,
                  fontWeight: isActive ? 600 : 400,
                  display: "flex",
                  alignItems: "center",
                  gap: 5,
                }}
              >
                {label}
                {completedSteps > 0 && (
                  <span style={{
                    background: run.status === "completed" ? "#22c55e" : tc.accent,
                    color: "#fff",
                    borderRadius: "50%",
                    width: 16,
                    height: 16,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: 9,
                    fontWeight: 700,
                  }}>
                    {completedSteps}
                  </span>
                )}
                {run.status === "running" && (
                  <span style={{
                    width: 6,
                    height: 6,
                    borderRadius: "50%",
                    background: "#22c55e",
                    animation: "pulse 1s infinite",
                  }} />
                )}
              </button>
            );
          })}
        </div>
      )}

      {/* Pannello del run attivo */}
      {activeRunData && (
        <SingleRunPanel
          run={activeRunData.run}
          steps={activeRunData.steps}
          tc={tc}
          t={t}
          onConfirm={onConfirm}
          label={isMulti ? activeRunData.label : ""}
          streamingToken={activeRunData.run.runId === agentRun.runId ? streamingToken : undefined}
        />
      )}
    </div>
  );
}
