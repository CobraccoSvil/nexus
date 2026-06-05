/* eslint-disable @typescript-eslint/ban-ts-comment */
// @ts-nocheck
"use client";

/**
 * SubagentDefinitionsEditor — PR-4 CRUD per nexus_subagent_definitions.
 */

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import {
  listSubagentDefinitions,
  upsertSubagentDefinition,
  deleteSubagentDefinition,
  listSubagentRuns,
  type SubagentDefinition,
  type OrchestratorSubagentRun,
} from "../../lib/api-client";

interface EditState {
  kind: string;
  description: string;
  promptKey: string;
  toolWhitelistCsv: string;
  modelPurpose: string;
  maxIterations: string;
  timeoutS: string;
  isBackground: boolean;
  isEnabled: boolean;
}

function emptyEdit(): EditState {
  return {
    kind: "",
    description: "",
    promptKey: "",
    toolWhitelistCsv: "list_files,read_file,search_in_files",
    modelPurpose: "planner",
    maxIterations: "25",
    timeoutS: "300",
    isBackground: false,
    isEnabled: true,
  };
}

export function SubagentDefinitionsEditor() {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();
  const [defs, setDefs] = useState<SubagentDefinition[]>([]);
  const [editing, setEditing] = useState<EditState | null>(null);
  const [recentRuns, setRecentRuns] = useState<Record<string, OrchestratorSubagentRun[]>>({});
  const [expanded, setExpanded] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const r = await listSubagentDefinitions();
      setDefs(r.definitions);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    }
  }, []);

  useEffect(() => { void reload(); }, [reload]);

  const toggleExpand = async (kind: string) => {
    if (expanded === kind) {
      setExpanded(null);
      return;
    }
    setExpanded(kind);
    if (!recentRuns[kind]) {
      try {
        const r = await listSubagentRuns({ kind, limit: 20 });
        setRecentRuns((m) => ({ ...m, [kind]: r.runs }));
      } catch (e) {
        setError(e instanceof Error ? e.message : "Errore caricamento runs");
      }
    }
  };

  const handleSave = async () => {
    if (!editing) return;
    if (!editing.kind || !editing.promptKey) {
      setError("kind e promptKey sono obbligatori");
      return;
    }
    try {
      await upsertSubagentDefinition({
        kind: editing.kind,
        description: editing.description || null,
        prompt_key: editing.promptKey,
        tool_whitelist: editing.toolWhitelistCsv.split(",").map((s) => s.trim()).filter(Boolean),
        model_purpose: editing.modelPurpose,
        max_iterations: Number(editing.maxIterations) || 25,
        timeout_s: Number(editing.timeoutS) || 300,
        is_background: editing.isBackground,
        is_enabled: editing.isEnabled,
      });
      setEditing(null);
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore save");
    }
  };

  const startEdit = (def?: SubagentDefinition) => {
    if (!def) {
      setEditing(emptyEdit());
      return;
    }
    setEditing({
      kind: def.kind,
      description: def.description ?? "",
      promptKey: def.promptKey,
      toolWhitelistCsv: def.toolWhitelist.join(","),
      modelPurpose: def.modelPurpose,
      maxIterations: String(def.maxIterations),
      timeoutS: String(def.timeoutS),
      isBackground: def.isBackground,
      isEnabled: def.isEnabled,
    });
  };

  const handleDelete = async (kind: string) => {
    const ok = await confirmDialog({
      title: "Disabilita sub-agent",
      message: `Disabilitare il sub-agent kind '${kind}'?`,
      danger: true,
      confirmLabel: "Disabilita",
      cancelLabel: "Annulla",
    });
    if (!ok) return;
    try {
      await deleteSubagentDefinition(kind);
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore delete");
    }
  };

  return (
    <div style={{ padding: 24, color: tc.text, maxWidth: 1200 }}>
      <header style={{ display: "flex", justifyContent: "space-between", marginBottom: 24, alignItems: "center" }}>
        <div>
          <h1 style={{ margin: 0 }}>Sub-agent kinds</h1>
          <div style={{ fontSize: 12, color: tc.textMuted }}>Custom sub-agent definitions. Project YAML overrides applicano su <code>.nexus/agents/&lt;kind&gt;.md</code>.</div>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <a href="/admin/orchestrator" style={{ padding: "6px 12px", border: `1px solid ${tc.border}`, borderRadius: 4, textDecoration: "none", color: tc.text }}>← Orchestrator</a>
          <button onClick={() => startEdit()} style={{ padding: "6px 12px", background: "#2563eb", color: "white", border: "none", borderRadius: 4, cursor: "pointer" }}>+ Nuovo kind</button>
        </div>
      </header>

      {error && <div style={{ background: "#fee2e2", color: "#991b1b", padding: 8, borderRadius: 4, marginBottom: 16 }}>{error}</div>}

      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12, background: tc.bgCard }}>
        <thead>
          <tr style={{ borderBottom: `1px solid ${tc.border}` }}>
            <th style={{ textAlign: "left", padding: 8 }}>Kind</th>
            <th style={{ textAlign: "left", padding: 8 }}>Purpose</th>
            <th style={{ textAlign: "left", padding: 8 }}>Tools</th>
            <th style={{ textAlign: "right", padding: 8 }}>Max iter</th>
            <th style={{ textAlign: "right", padding: 8 }}>Timeout</th>
            <th style={{ textAlign: "center", padding: 8 }}>BG</th>
            <th style={{ textAlign: "center", padding: 8 }}>Enabled</th>
            <th style={{ padding: 8 }}></th>
          </tr>
        </thead>
        <tbody>
          {defs.map((d) => (
            <>
              <tr key={d.kind} style={{ borderBottom: `1px solid ${tc.border}33` }}>
                <td style={{ padding: 8, fontWeight: 600 }}>
                  <button onClick={() => toggleExpand(d.kind)} style={{ background: "none", border: "none", cursor: "pointer", color: tc.text, fontWeight: 600 }}>
                    {expanded === d.kind ? "▼" : "▶"} {d.kind}
                  </button>
                </td>
                <td style={{ padding: 8 }}>{d.modelPurpose}</td>
                <td style={{ padding: 8, fontSize: 11, color: tc.textMuted }}>{d.toolWhitelist.join(", ")}</td>
                <td style={{ padding: 8, textAlign: "right" }}>{d.maxIterations}</td>
                <td style={{ padding: 8, textAlign: "right" }}>{d.timeoutS}s</td>
                <td style={{ padding: 8, textAlign: "center" }}>{d.isBackground ? "✓" : ""}</td>
                <td style={{ padding: 8, textAlign: "center" }}>{d.isEnabled ? "✓" : "✗"}</td>
                <td style={{ padding: 8 }}>
                  <button onClick={() => startEdit(d)} style={{ padding: "2px 8px", background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, cursor: "pointer", marginRight: 4, fontSize: 11 }}>Edit</button>
                  <button onClick={() => handleDelete(d.kind)} style={{ padding: "2px 8px", background: "#fee2e2", color: "#991b1b", border: "1px solid #fca5a5", borderRadius: 4, cursor: "pointer", fontSize: 11 }}>Disable</button>
                </td>
              </tr>
              {expanded === d.kind && (
                <tr>
                  <td colSpan={8} style={{ padding: 12, background: `${tc.border}11` }}>
                    <div style={{ fontSize: 11, marginBottom: 8 }}><strong>Description:</strong> {d.description ?? "-"}</div>
                    <div style={{ fontSize: 11, marginBottom: 8 }}><strong>Prompt key:</strong> <code>{d.promptKey}</code></div>
                    <h4 style={{ marginTop: 8, marginBottom: 4 }}>Recent runs</h4>
                    {(recentRuns[d.kind] ?? []).length === 0 ? (
                      <div style={{ color: tc.textMuted, fontSize: 11 }}>Nessuna run recente.</div>
                    ) : (
                      <ul style={{ fontSize: 11, paddingLeft: 16, margin: 0 }}>
                        {(recentRuns[d.kind] ?? []).map((r) => (
                          <li key={r.id} style={{ marginBottom: 2 }}>
                            <code>{r.id.slice(0, 8)}</code> {r.status} · iter={r.iterations} · ${r.costUsd.toFixed(4)} · {r.createdAt?.slice(11, 19)}
                          </li>
                        ))}
                      </ul>
                    )}
                  </td>
                </tr>
              )}
            </>
          ))}
        </tbody>
      </table>

      {editing && (
        <div style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,0.6)", zIndex: 200, display: "flex", justifyContent: "center", alignItems: "center" }} onClick={() => setEditing(null)}>
          <div onClick={(e) => e.stopPropagation()} style={{ background: tc.bg, color: tc.text, padding: 24, borderRadius: 8, width: 560, maxHeight: "90vh", overflowY: "auto" }}>
            <h2 style={{ marginTop: 0 }}>{editing.kind ? `Edit '${editing.kind}'` : "Nuovo sub-agent kind"}</h2>
            <div style={{ display: "grid", gap: 12 }}>
              <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                <span>Kind (slug a-z_)</span>
                <input value={editing.kind} onChange={(e) => setEditing((s) => s && { ...s, kind: e.target.value })} style={fieldStyle(tc)} />
              </label>
              <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                <span>Description (usato per auto-delegation by description)</span>
                <textarea rows={2} value={editing.description} onChange={(e) => setEditing((s) => s && { ...s, description: e.target.value })} style={fieldStyle(tc)} />
              </label>
              <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                <span>Prompt key (deve esistere in nexus_prompt_templates)</span>
                <input value={editing.promptKey} onChange={(e) => setEditing((s) => s && { ...s, promptKey: e.target.value })} style={fieldStyle(tc)} />
              </label>
              <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                <span>Tool whitelist (CSV)</span>
                <input value={editing.toolWhitelistCsv} onChange={(e) => setEditing((s) => s && { ...s, toolWhitelistCsv: e.target.value })} style={fieldStyle(tc)} />
              </label>
              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 8 }}>
                <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                  <span>Model purpose</span>
                  <input value={editing.modelPurpose} onChange={(e) => setEditing((s) => s && { ...s, modelPurpose: e.target.value })} style={fieldStyle(tc)} />
                </label>
                <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                  <span>Max iter</span>
                  <input value={editing.maxIterations} onChange={(e) => setEditing((s) => s && { ...s, maxIterations: e.target.value })} style={fieldStyle(tc)} />
                </label>
                <label style={{ display: "flex", flexDirection: "column", gap: 2, fontSize: 12 }}>
                  <span>Timeout (s)</span>
                  <input value={editing.timeoutS} onChange={(e) => setEditing((s) => s && { ...s, timeoutS: e.target.value })} style={fieldStyle(tc)} />
                </label>
              </div>
              <div style={{ display: "flex", gap: 16, fontSize: 12 }}>
                <label style={{ display: "flex", gap: 4, alignItems: "center" }}>
                  <input type="checkbox" checked={editing.isBackground} onChange={(e) => setEditing((s) => s && { ...s, isBackground: e.target.checked })} />
                  Is background
                </label>
                <label style={{ display: "flex", gap: 4, alignItems: "center" }}>
                  <input type="checkbox" checked={editing.isEnabled} onChange={(e) => setEditing((s) => s && { ...s, isEnabled: e.target.checked })} />
                  Is enabled
                </label>
              </div>
              <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
                <button onClick={() => setEditing(null)} style={{ padding: "6px 12px", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, cursor: "pointer" }}>Annulla</button>
                <button onClick={handleSave} style={{ padding: "6px 12px", background: "#2563eb", color: "white", border: "none", borderRadius: 4, cursor: "pointer" }}>Save</button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function fieldStyle(tc: ReturnType<typeof useThemeColors>): React.CSSProperties {
  return { padding: "4px 8px", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, fontSize: 12 };
}
