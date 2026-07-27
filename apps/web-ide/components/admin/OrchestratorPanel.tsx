/* eslint-disable @typescript-eslint/ban-ts-comment */
// @ts-nocheck
"use client";

/**
 * OrchestratorPanel — PR-4 admin per Plan/Act/Verify + Sub-agents.
 *
 * Sezioni:
 *  - Toggle feature flags (orchestrator.*) tramite settings API
 *  - Lista plan recenti (run_id, project, todos done/total, subagent count, costo aggregato)
 *  - Drill-down su plan → PlanInspector
 *  - Link a /admin/orchestrator/subagents per CRUD kind
 */

import { useEffect, useState, useCallback } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  listOrchestratorPlans,
  type OrchestratorPlanSummary,
  listAdminSettings,
  updateAdminSetting,
} from "../../lib/api-client";
import { PlanInspector } from "./PlanInspector";

const TOGGLES: { key: string; label: string; hint: string }[] = [
  { key: "orchestrator.plan_phase_enabled", label: "Plan phase", hint: "Attiva planner_node che produce TODO list prima dell'executor" },
  { key: "orchestrator.verifier_enabled", label: "Verifier", hint: "Verifier deterministico DoD su acceptance criteria post run" },
  { key: "orchestrator.subagents_enabled", label: "Sub-agents", hint: "Permette il tool dispatch_subagent (context isolato)" },
  { key: "orchestrator.clarifying_questions_enabled", label: "Clarifying questions", hint: "Codex pattern: il planner emette domande pre-flight su task ambigui" },
  { key: "orchestrator.auto_delegation_enabled", label: "Auto-delegation by description", hint: "Cursor pattern: inserisce <available_subagents> nel system_text" },
  { key: "orchestrator.subagent_project_override_enabled", label: "Project YAML overrides", hint: "Permette .nexus/agents/<kind>.md di shadow-are le definition DB" },
  { key: "orchestrator.subagent_parallel_in_round", label: "Parallel sub-agents per turn", hint: "Permette N dispatch_subagent nello stesso turno LLM" },
  // Il toggle "DAG parallel execution" (orchestrator.dag_parallel_enabled) e'
  // stato TOLTO da qui. Il suo ramo esiste in route_after_planner, ma il campo
  // non viene popolato dal DB di proposito: senza il dispatch DAG nell'executor
  // abilitarlo lascerebbe i todo orfani. Il toggle prometteva quindi
  // un'esecuzione parallela che non poteva attivare, e l'unico effetto possibile
  // di accenderlo sarebbe stato dirottare il routing dopo il planner.
  // I todo in parallelo li esegue gia' il todo_runner, per un'altra strada.
  { key: "orchestrator.dag_topological_enabled", label: "DAG topological order", hint: "Il verifier sceglie il prossimo todo rispettando depends_on (ordine topologico) invece del solo seq lineare" },
  { key: "orchestrator.dag_verify_layer", label: "DAG verify per layer", hint: "Dopo ogni ondata parallela verifica i todo completati prima di procedere al layer successivo" },
];

const NUMS: { key: string; label: string; type?: "int" | "float" | "csv" }[] = [
  { key: "orchestrator.max_verify_cycles", label: "Max verify cycles", type: "int" },
  { key: "orchestrator.max_plan_revisions", label: "Max plan revisions", type: "int" },
  { key: "orchestrator.todo_reminder_every_n_steps", label: "TODO reminder every N steps", type: "int" },
  { key: "orchestrator.todo_reminder_min_todos", label: "TODO reminder min todos", type: "int" },
  { key: "orchestrator.max_parallel_subagents", label: "Max parallel sub-agents", type: "int" },
  { key: "orchestrator.dag_max_parallel", label: "DAG max parallel per wave", type: "int" },
  { key: "orchestrator.subagent_max_depth", label: "Sub-agent max depth", type: "int" },
  { key: "orchestrator.subagent_cost_cap_per_run_usd", label: "Sub-agent cost cap per run (USD)", type: "float" },
  { key: "orchestrator.verifier_timeout_s", label: "Verifier timeout (s)", type: "float" },
  { key: "orchestrator.subagent_kinds_whitelist", label: "Sub-agent kinds whitelist (CSV)", type: "csv" },
  { key: "orchestrator.plan_intents", label: "Plan intents (CSV)", type: "csv" },
  { key: "orchestrator.plan_behavior_modes", label: "Plan behavior modes (CSV)", type: "csv" },
];

export function OrchestratorPanel() {
  const tc = useThemeColors();
  const [settings, setSettings] = useState<Record<string, string>>({});
  const [plans, setPlans] = useState<OrchestratorPlanSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [all, recent] = await Promise.all([
        listAdminSettings(),
        listOrchestratorPlans({ limit: 50 }),
      ]);
      const map: Record<string, string> = {};
      for (const s of all.settings || []) {
        if (s.key.startsWith("orchestrator.")) map[s.key] = String(s.value);
      }
      setSettings(map);
      setPlans(recent.plans || []);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const handleToggle = async (key: string, current: string) => {
    const next = current === "true" ? "false" : "true";
    try {
      await updateAdminSetting(key, next);
      setSettings((s) => ({ ...s, [key]: next }));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore update setting");
    }
  };

  const handleNumChange = async (key: string, value: string) => {
    try {
      await updateAdminSetting(key, value);
      setSettings((s) => ({ ...s, [key]: value }));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore update setting");
    }
  };

  return (
    <div style={{ padding: 24, color: tc.text, maxWidth: 1200 }}>
      <header style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 24 }}>
        <h1 style={{ margin: 0 }}>Orchestrator</h1>
        <div style={{ display: "flex", gap: 8 }}>
          <a href="/admin/orchestrator/subagents" style={{ padding: "6px 12px", border: `1px solid ${tc.border}`, borderRadius: 4, textDecoration: "none", color: tc.text }}>Sub-agents kinds</a>
          <button onClick={reload} style={{ padding: "6px 12px", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, cursor: "pointer" }}>Reload</button>
        </div>
      </header>

      {error && <div style={{ background: "#fee2e2", color: "#991b1b", padding: 8, borderRadius: 4, marginBottom: 16 }}>{error}</div>}
      {loading && <div style={{ color: tc.textMuted }}>Loading...</div>}

      <section style={{ marginBottom: 32, background: tc.bgCard, padding: 16, borderRadius: 8, border: `1px solid ${tc.border}` }}>
        <h2 style={{ marginTop: 0 }}>Feature flags</h2>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: 12 }}>
          {TOGGLES.map((t) => {
            const val = settings[t.key] ?? "false";
            const on = val === "true";
            return (
              <label key={t.key} style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer", padding: 8, border: `1px solid ${tc.border}`, borderRadius: 4 }} title={t.hint}>
                <input type="checkbox" checked={on} onChange={() => handleToggle(t.key, val)} />
                <div>
                  <div style={{ fontSize: 13, fontWeight: 600 }}>{t.label}</div>
                  <div style={{ fontSize: 11, color: tc.textMuted }}>{t.key}</div>
                </div>
              </label>
            );
          })}
        </div>
      </section>

      <section style={{ marginBottom: 32, background: tc.bgCard, padding: 16, borderRadius: 8, border: `1px solid ${tc.border}` }}>
        <h2 style={{ marginTop: 0 }}>Numeric thresholds</h2>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))", gap: 12 }}>
          {NUMS.map((n) => (
            <label key={n.key} style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 12 }}>
              <span style={{ fontWeight: 600 }}>{n.label}</span>
              <span style={{ fontSize: 10, color: tc.textMuted }}>{n.key}</span>
              <input
                type="text"
                value={settings[n.key] ?? ""}
                onChange={(e) => setSettings((s) => ({ ...s, [n.key]: e.target.value }))}
                onBlur={(e) => handleNumChange(n.key, e.target.value)}
                style={{ padding: "4px 8px", background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
              />
            </label>
          ))}
        </div>
      </section>

      <section style={{ background: tc.bgCard, padding: 16, borderRadius: 8, border: `1px solid ${tc.border}` }}>
        <h2 style={{ marginTop: 0 }}>Plans recenti</h2>
        {plans.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>Nessun plan registrato. Lancia un run con plan_phase_enabled=true.</div>
        ) : (
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}>
            <thead>
              <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
                <th style={{ textAlign: "left", padding: 6 }}>Run ID</th>
                <th style={{ textAlign: "left", padding: 6 }}>Project</th>
                <th style={{ textAlign: "left", padding: 6 }}>Planner</th>
                <th style={{ textAlign: "right", padding: 6 }}>Todos</th>
                <th style={{ textAlign: "right", padding: 6 }}>Verifier</th>
                <th style={{ textAlign: "right", padding: 6 }}>Sub-agents</th>
                <th style={{ textAlign: "left", padding: 6 }}>Creato</th>
                <th style={{ padding: 6 }}></th>
              </tr>
            </thead>
            <tbody>
              {plans.map((p) => (
                <tr key={p.runId} style={{ borderBottom: `1px solid ${tc.border}33` }}>
                  <td style={{ padding: 6, fontFamily: "var(--font-mono)" }}>{p.runId.slice(0, 8)}</td>
                  <td style={{ padding: 6, fontFamily: "var(--font-mono)" }}>{p.projectId.slice(0, 8)}</td>
                  <td style={{ padding: 6 }}>{p.plannerModel ?? "-"}</td>
                  <td style={{ padding: 6, textAlign: "right" }}>{p.todosDone}/{p.todosTotal}</td>
                  <td style={{ padding: 6, textAlign: "right" }}>{p.verifierRuns}</td>
                  <td style={{ padding: 6, textAlign: "right" }}>{p.subagentRuns}</td>
                  <td style={{ padding: 6 }}>{p.createdAt?.slice(0, 19).replace("T", " ")}</td>
                  <td style={{ padding: 6 }}>
                    <button onClick={() => setSelectedRunId(p.runId)} style={{ padding: "2px 8px", background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, cursor: "pointer", fontSize: 11 }}>
                      Inspect
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      {selectedRunId && <PlanInspector runId={selectedRunId} onClose={() => setSelectedRunId(null)} />}
    </div>
  );
}
