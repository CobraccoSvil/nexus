"use client";

import { useEffect, useState } from "react";
import {
  getMyProjects,
  listLearnedInstructions,
  patchLearnedInstruction,
  distillLearnedInstructions,
  type LearnedRule,
  type LearnedStatus,
  type UserProjectSummary,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";

type StatusFilter = "all" | LearnedStatus;
const STATUS_FILTERS: StatusFilter[] = ["proposed", "active", "rejected", "retired", "all"];
const CATEGORIES = ["convention", "preference", "environment", "tooling", "process"];

export default function AdminLearnedInstructionsPage() {
  const tc = useThemeColors();
  const [projects, setProjects] = useState<UserProjectSummary[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string>("");
  const [rules, setRules] = useState<LearnedRule[]>([]);
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("proposed");
  const [busy, setBusy] = useState<"load" | "save" | "distill" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editText, setEditText] = useState("");

  useEffect(() => {
    const bootstrap = async () => {
      setBusy("load");
      setError(null);
      try {
        const response = await getMyProjects();
        setProjects(response.projects ?? []);
        if (response.projects?.length) setSelectedProjectId(response.projects[0].id);
      } catch (e) {
        setError(e instanceof Error ? e.message : "Impossibile caricare i progetti.");
      } finally {
        setBusy(null);
      }
    };
    void bootstrap();
  }, []);

  const reload = async (projectId: string, filter: StatusFilter) => {
    setBusy("load");
    setError(null);
    try {
      const response = await listLearnedInstructions(projectId, filter);
      setRules(response.data ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Impossibile caricare le regole.");
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    if (!selectedProjectId) return;
    void reload(selectedProjectId, statusFilter);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedProjectId, statusFilter]);

  const applyPatch = async (id: string, body: { status?: LearnedStatus; rule_text?: string; category?: string }) => {
    setBusy("save");
    setError(null);
    try {
      await patchLearnedInstruction(id, body);
      setEditingId(null);
      await reload(selectedProjectId, statusFilter);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Operazione fallita.");
      setBusy(null);
    }
  };

  const handleDistill = async () => {
    if (!selectedProjectId) return;
    setBusy("distill");
    setError(null);
    setNotice(null);
    try {
      const res = await distillLearnedInstructions(selectedProjectId);
      setNotice(
        res.ok
          ? `Distillazione completata (${res.applied ?? 0} operazioni applicate).`
          : `Distillazione: ${res.error ?? "nessun risultato"}.`,
      );
      await reload(selectedProjectId, statusFilter);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Distillazione fallita.");
      setBusy(null);
    }
  };

  const statusColor = (s: LearnedStatus): string =>
    s === "active" ? tc.success : s === "rejected" ? tc.error : s === "retired" ? tc.textMuted : tc.warning;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <AdminPageHeader
        title="Learned Instructions"
        description="Regole durature di progetto distillate dall'esperienza operativa. Le regole 'active' vengono iniettate in ogni run; rivedi qui le 'proposed'."
        action={
          <button
            onClick={() => void handleDistill()}
            disabled={!selectedProjectId || busy !== null}
            style={primaryButton(tc, busy === "distill")}
          >
            {busy === "distill" ? "Distillazione..." : "Distilla ora"}
          </button>
        }
      />

      {error ? <div style={banner(tc, tc.error)}>{error}</div> : null}
      {notice ? <div style={banner(tc, tc.accent)}>{notice}</div> : null}

      <div style={panel(tc)}>
        <label style={{ fontSize: 12, color: tc.textMuted, display: "block", marginBottom: 6 }}>Progetto</label>
        <select
          value={selectedProjectId}
          onChange={(e) => setSelectedProjectId(e.target.value)}
          style={inputStyle(tc)}
          disabled={busy === "load" || projects.length === 0}
        >
          {projects.length === 0 ? <option value="">Nessun progetto</option> : null}
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </div>

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {STATUS_FILTERS.map((s) => {
          const active = statusFilter === s;
          return (
            <button
              key={s}
              onClick={() => setStatusFilter(s)}
              style={{
                padding: "6px 12px",
                borderRadius: 6,
                border: `1px solid ${active ? tc.accent : tc.border}`,
                background: active ? tc.accentBg : tc.bgInput,
                color: active ? tc.accent : tc.text,
                fontSize: 12,
                cursor: "pointer",
                fontFamily: "inherit",
              }}
            >
              {s}
            </button>
          );
        })}
      </div>

      <div style={{ borderRadius: 10, border: `1px solid ${tc.border}`, background: tc.bgCard, overflow: "hidden" }}>
        {rules.length === 0 ? (
          <div style={{ padding: "40px 24px", textAlign: "center", color: tc.textMuted, fontSize: 13 }}>
            {busy === "load" ? "Caricamento..." : `Nessuna regola con status "${statusFilter}".`}
          </div>
        ) : (
          <table style={{ width: "100%", borderCollapse: "collapse" }}>
            <thead>
              <tr style={{ borderBottom: `1px solid ${tc.border}`, background: tc.bgHover }}>
                {["Categoria", "Regola", "Conf.", "Stato", "Azioni"].map((h) => (
                  <th key={h} style={{ padding: "10px 14px", textAlign: "left", fontSize: 12, fontWeight: 600, color: tc.textMuted }}>
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rules.map((rule) => (
                <tr key={rule.id} style={{ borderBottom: `1px solid ${tc.border}`, verticalAlign: "top" }}>
                  <td style={{ padding: "10px 14px", fontSize: 12, color: tc.textSecondary }}>
                    {editingId === rule.id ? (
                      <select
                        defaultValue={rule.category}
                        id={`cat-${rule.id}`}
                        style={{ ...inputStyle(tc), fontSize: 12 }}
                      >
                        {CATEGORIES.map((c) => (
                          <option key={c} value={c}>
                            {c}
                          </option>
                        ))}
                      </select>
                    ) : (
                      rule.category
                    )}
                  </td>
                  <td style={{ padding: "10px 14px", fontSize: 13, color: tc.text, maxWidth: 420 }}>
                    {editingId === rule.id ? (
                      <textarea
                        value={editText}
                        onChange={(e) => setEditText(e.target.value)}
                        style={{ ...inputStyle(tc), minHeight: 56, resize: "vertical" }}
                      />
                    ) : (
                      <>
                        <div style={{ whiteSpace: "pre-wrap" }}>{rule.ruleText}</div>
                        {rule.rationale ? (
                          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 4 }}>{rule.rationale}</div>
                        ) : null}
                        {rule.manuallyEdited ? (
                          <div style={{ fontSize: 10, color: tc.accent, marginTop: 2 }}>modificata a mano</div>
                        ) : null}
                      </>
                    )}
                  </td>
                  <td style={{ padding: "10px 14px", fontSize: 12, color: tc.text }}>
                    {Math.round((rule.confidence ?? 0) * 100)}%
                  </td>
                  <td style={{ padding: "10px 14px", fontSize: 12 }}>
                    <span
                      style={{
                        display: "inline-block",
                        padding: "2px 8px",
                        borderRadius: 999,
                        fontSize: 11,
                        color: statusColor(rule.status),
                        border: `1px solid ${statusColor(rule.status)}`,
                      }}
                    >
                      {rule.status}
                    </span>
                  </td>
                  <td style={{ padding: "10px 14px", fontSize: 12 }}>
                    <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                      {editingId === rule.id ? (
                        <>
                          <button
                            onClick={() => {
                              const catEl = document.getElementById(`cat-${rule.id}`) as HTMLSelectElement | null;
                              void applyPatch(rule.id, {
                                rule_text: editText.trim() || undefined,
                                category: catEl?.value,
                              });
                            }}
                            disabled={busy === "save"}
                            style={buttonStyle(tc)}
                          >
                            Salva
                          </button>
                          <button onClick={() => setEditingId(null)} style={buttonStyle(tc)}>
                            Annulla
                          </button>
                        </>
                      ) : (
                        <>
                          <button
                            onClick={() => {
                              setEditingId(rule.id);
                              setEditText(rule.ruleText);
                            }}
                            style={buttonStyle(tc)}
                          >
                            Modifica
                          </button>
                          {rule.status !== "active" ? (
                            <button
                              onClick={() => void applyPatch(rule.id, { status: "active" })}
                              disabled={busy === "save"}
                              style={buttonStyle(tc, tc.success)}
                            >
                              Attiva
                            </button>
                          ) : null}
                          {rule.status !== "rejected" ? (
                            <button
                              onClick={() => void applyPatch(rule.id, { status: "rejected" })}
                              disabled={busy === "save"}
                              style={buttonStyle(tc, tc.error)}
                            >
                              Rifiuta
                            </button>
                          ) : null}
                        </>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

type Tc = ReturnType<typeof useThemeColors>;

function panel(tc: Tc) {
  return { padding: 14, borderRadius: 10, border: `1px solid ${tc.border}`, background: tc.bgCard } as const;
}

function banner(tc: Tc, color: string) {
  return {
    padding: "10px 14px",
    borderRadius: 8,
    border: `1px solid ${color}`,
    color,
    background: tc.bgCard,
    fontSize: 13,
  } as const;
}

function inputStyle(tc: Tc) {
  return {
    width: "100%",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontSize: 13,
    fontFamily: "inherit",
    boxSizing: "border-box" as const,
  };
}

function buttonStyle(tc: Tc, color?: string) {
  return {
    padding: "5px 10px",
    borderRadius: 6,
    border: `1px solid ${color ?? tc.border}`,
    background: tc.bgInput,
    color: color ?? tc.text,
    cursor: "pointer",
    fontSize: 11,
    fontFamily: "inherit",
  } as const;
}

function primaryButton(tc: Tc, busy: boolean) {
  return {
    padding: "8px 14px",
    borderRadius: 8,
    border: `1px solid ${tc.accent}`,
    background: busy ? tc.bgInput : tc.accentBg,
    color: tc.accent,
    cursor: busy ? "default" : "pointer",
    fontSize: 13,
    fontFamily: "inherit",
  } as const;
}
