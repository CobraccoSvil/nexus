/* eslint-disable @typescript-eslint/ban-ts-comment */
// @ts-nocheck
"use client";

/**
 * PlanInspector — drawer dettaglio plan (PR-4).
 *
 * Mostra:
 *  - lista todos con status
 *  - verifier runs (cycle, passed, criteria_results)
 *  - albero sub-agent runs annidati (depth, kind, cost rollup)
 */

import { useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { getOrchestratorPlan, type OrchestratorPlanDetail } from "../../lib/api-client";

const STATUS_COLORS: Record<string, string> = {
  pending: "#6b7280",
  in_progress: "#2563eb",
  completed: "#16a34a",
  blocked: "#dc2626",
  skipped: "#a16207",
};

export function PlanInspector({ runId, onClose }: { runId: string; onClose: () => void }) {
  const tc = useThemeColors();
  const [plan, setPlan] = useState<OrchestratorPlanDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancel = false;
    setLoading(true);
    getOrchestratorPlan(runId)
      .then((d) => { if (!cancel) setPlan(d); })
      .catch((e) => { if (!cancel) setError(e instanceof Error ? e.message : "Errore"); })
      .finally(() => { if (!cancel) setLoading(false); });
    return () => { cancel = true; };
  }, [runId]);

  const totalSubagentCost = plan?.subagentRuns.reduce((acc, r) => acc + r.costUsd, 0) ?? 0;
  const totalSubagentTokens = plan?.subagentRuns.reduce((acc, r) => acc + r.tokensPrompt + r.tokensCompletion, 0) ?? 0;

  return (
    <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.5)", zIndex: 100, display: "flex", justifyContent: "flex-end" }} onClick={onClose}>
      <div onClick={(e) => e.stopPropagation()} style={{ width: 720, maxWidth: "90vw", height: "100vh", background: tc.bg, color: tc.text, overflowY: "auto", padding: 24, boxShadow: "-4px 0 16px rgba(0,0,0,0.3)" }}>
        <header style={{ display: "flex", justifyContent: "space-between", marginBottom: 16, borderBottom: `1px solid ${tc.border}`, paddingBottom: 12 }}>
          <div>
            <h2 style={{ margin: 0 }}>Plan {runId.slice(0, 8)}</h2>
            <div style={{ fontSize: 11, color: tc.textMuted, fontFamily: "monospace" }}>{runId}</div>
          </div>
          <button onClick={onClose} style={{ padding: "4px 12px", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, cursor: "pointer" }}>Close</button>
        </header>

        {loading && <div style={{ color: tc.textMuted }}>Loading...</div>}
        {error && <div style={{ background: "#fee2e2", color: "#991b1b", padding: 8, borderRadius: 4 }}>{error}</div>}
        {plan && (
          <>
            <section style={{ marginBottom: 24, fontSize: 12 }}>
              <div><strong>Project:</strong> <code>{plan.projectId}</code></div>
              <div><strong>Planner model:</strong> {plan.plannerModel ?? "-"}</div>
              <div><strong>Score:</strong> {plan.score ?? "-"}</div>
              <div><strong>Created:</strong> {plan.createdAt?.slice(0, 19).replace("T", " ")}</div>
              <div><strong>Approved:</strong> {plan.approvedAt?.slice(0, 19).replace("T", " ") ?? "-"}</div>
            </section>

            <section style={{ marginBottom: 24 }}>
              <h3 style={{ marginBottom: 8 }}>Todos ({plan.todos.length})</h3>
              <ol style={{ paddingLeft: 24, fontSize: 13, margin: 0 }}>
                {plan.todos.map((t) => (
                  <li key={t.id} style={{ marginBottom: 4 }}>
                    <span style={{ display: "inline-block", minWidth: 90, fontSize: 10, padding: "1px 6px", borderRadius: 3, background: STATUS_COLORS[t.status] ?? "#888", color: "white", marginRight: 8 }}>{t.status}</span>
                    {t.content}
                    {t.verifyFailures > 0 && <span style={{ marginLeft: 8, color: "#dc2626", fontSize: 11 }}>{t.verifyFailures} verifier failures</span>}
                  </li>
                ))}
              </ol>
            </section>

            <section style={{ marginBottom: 24 }}>
              <h3 style={{ marginBottom: 8 }}>Verifier runs ({plan.verifierRuns.length})</h3>
              {plan.verifierRuns.length === 0 ? (
                <div style={{ color: tc.textMuted, fontSize: 12 }}>Nessuna verifier run.</div>
              ) : (
                <table style={{ width: "100%", fontSize: 11, borderCollapse: "collapse" }}>
                  <thead>
                    <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
                      <th style={{ textAlign: "left", padding: 4 }}>Cycle</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Todo</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Passed</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Duration</th>
                      <th style={{ textAlign: "left", padding: 4 }}>When</th>
                    </tr>
                  </thead>
                  <tbody>
                    {plan.verifierRuns.map((v) => (
                      <tr key={v.id} style={{ borderBottom: `1px solid ${tc.border}33` }}>
                        <td style={{ padding: 4 }}>{v.cycle}</td>
                        <td style={{ padding: 4, fontFamily: "monospace" }}>{v.todoId?.slice(0, 8) ?? "-"}</td>
                        <td style={{ padding: 4, color: v.passed ? "#16a34a" : "#dc2626" }}>{v.passed ? "PASS" : "FAIL"}</td>
                        <td style={{ padding: 4 }}>{v.durationMs ? `${v.durationMs}ms` : "-"}</td>
                        <td style={{ padding: 4 }}>{v.createdAt?.slice(11, 19)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>

            <section style={{ marginBottom: 24 }}>
              <h3 style={{ marginBottom: 8 }}>Sub-agent runs ({plan.subagentRuns.length}) — total ${totalSubagentCost.toFixed(4)} / {totalSubagentTokens.toLocaleString()} tok</h3>
              {plan.subagentRuns.length === 0 ? (
                <div style={{ color: tc.textMuted, fontSize: 12 }}>Nessuna sub-agent run.</div>
              ) : (
                <table style={{ width: "100%", fontSize: 11, borderCollapse: "collapse" }}>
                  <thead>
                    <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
                      <th style={{ textAlign: "left", padding: 4 }}>Kind</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Status</th>
                      <th style={{ textAlign: "right", padding: 4 }}>Iter</th>
                      <th style={{ textAlign: "right", padding: 4 }}>Tokens</th>
                      <th style={{ textAlign: "right", padding: 4 }}>Cost</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Source</th>
                      <th style={{ textAlign: "left", padding: 4 }}>Depth</th>
                    </tr>
                  </thead>
                  <tbody>
                    {plan.subagentRuns.map((s) => (
                      <tr key={s.id} style={{ borderBottom: `1px solid ${tc.border}33` }}>
                        <td style={{ padding: 4, fontWeight: 600 }}>{s.kind}</td>
                        <td style={{ padding: 4 }}>{s.status}</td>
                        <td style={{ padding: 4, textAlign: "right" }}>{s.iterations}</td>
                        <td style={{ padding: 4, textAlign: "right" }}>{(s.tokensPrompt + s.tokensCompletion).toLocaleString()}</td>
                        <td style={{ padding: 4, textAlign: "right" }}>${s.costUsd.toFixed(4)}</td>
                        <td style={{ padding: 4, color: s.source === "project_override" ? "#0891b2" : tc.textMuted }}>{s.source}</td>
                        <td style={{ padding: 4 }}>{s.depth}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
