"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import {
  listPromptTemplates,
  getPromptTemplate,
  updatePromptTemplate,
  disablePromptTemplate,
  enablePromptTemplate,
  aiSuggestPromptTemplate,
  getPromptTools,
  updatePromptTools,
  getAvailableMcpTools,
  previewPromptTemplate,
  type PromptTemplate,
  type PromptTemplateHistory,
  type PromptMcpTool,
  type PromptToolsResponse,
  type AvailableMcpTool,
  type PromptPreviewResponse,
  batchAssignAllTools,
} from "../../../lib/api-client";

export default function PromptsAdminPage() {
  const tc = useThemeColors();
  const [templates, setTemplates] = useState<PromptTemplate[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<{ template: PromptTemplate; history: PromptTemplateHistory[] } | null>(null);
  const [editContent, setEditContent] = useState("");
  const [changeNote, setChangeNote] = useState("");
  const [saving, setSaving] = useState(false);
  const [aiOpen, setAiOpen] = useState(false);
  const [aiInstruction, setAiInstruction] = useState("");
  const [aiBusy, setAiBusy] = useState(false);
  const [aiSuggestion, setAiSuggestion] = useState<string | null>(null);
  const [aiAutoAssigned, setAiAutoAssigned] = useState<number>(0);
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchResult, setBatchResult] = useState<{ processed: number; assigned: number; errors: number } | null>(null);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [promptTools, setPromptTools] = useState<PromptToolsResponse | null>(null);
  const [availableTools, setAvailableTools] = useState<AvailableMcpTool[]>([]);
  const [toolsLoading, setToolsLoading] = useState(false);
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(new Set());
  const [showAnalytics, setShowAnalytics] = useState(false);
  // Preview rendering placeholder ({{lang_hint}}, {{type_hint}}, {{repo_summary}})
  const [preview, setPreview] = useState<PromptPreviewResponse | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewIntent, setPreviewIntent] = useState<string>("chat");
  const [previewLang, setPreviewLang] = useState<string>("");

  // Reset preview ad ogni cambio di template selezionato
  useEffect(() => { setPreview(null); }, [selected]);

  const runPreview = async (key: string) => {
    setPreviewLoading(true);
    try {
      const res = await previewPromptTemplate(key, {
        intent: previewIntent || undefined,
        repo_lang: previewLang || undefined,
      });
      setPreview(res);
    } catch (e) {
      setPreview({
        key,
        schema_type: "error",
        rendered: e instanceof Error ? e.message : String(e),
        unresolved_placeholders: [],
      });
    } finally {
      setPreviewLoading(false);
    }
  };

  const toggleCategory = (cat: string) => {
    setExpandedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  };

  useEffect(() => {
    listPromptTemplates()
      .then((list) => setTemplates(list))
      .catch((e) => setError(e instanceof Error ? e.message : "Errore caricamento"))
      .finally(() => setLoading(false));
  }, []);

  const selectTemplate = async (key: string) => {
    setSelected(key);
    setDetail(null);
    setEditContent("");
    setAiOpen(false);
    setAiInstruction("");
    setAiSuggestion(null);
    setPromptTools(null);
    setToolsOpen(false);
    try {
      const d = await getPromptTemplate(key);
      setDetail(d);
      setEditContent(d.template.content);
      setChangeNote("");
      // Load tools
      setToolsLoading(true);
      try {
        const tools = await getPromptTools(key);
        setPromptTools(tools);
      } catch {
        // endpoint non ancora implementato — ignora
        setPromptTools({ assigned_tools: [], suggested_tools: [], available_tools: [] });
      } finally {
        setToolsLoading(false);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento template");
    }
  };

  const loadAvailableTools = async () => {
    if (availableTools.length > 0) return;
    try {
      const tools = await getAvailableMcpTools();
      setAvailableTools(tools);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento tools");
    }
  };

  const toggleToolSelection = (tool: PromptMcpTool) => {
    if (!promptTools) return;
    const isSelected = promptTools.assigned_tools.some(
      t => t.tool_name === tool.tool_name && t.tool_server === tool.tool_server
    );
    if (isSelected) {
      setPromptTools({
        ...promptTools,
        assigned_tools: promptTools.assigned_tools.filter(
          t => !(t.tool_name === tool.tool_name && t.tool_server === tool.tool_server)
        ),
      });
    } else {
      setPromptTools({
        ...promptTools,
        assigned_tools: [...promptTools.assigned_tools, tool],
      });
    }
  };

  const saveTools = async () => {
    if (!selected || !promptTools) return;
    try {
      await updatePromptTools(selected, promptTools.assigned_tools);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore salvataggio tools");
    }
  };

  const runBatchAssign = async () => {
    setBatchBusy(true);
    setBatchResult(null);
    try {
      const res = await batchAssignAllTools();
      setBatchResult(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore batch assign");
    } finally {
      setBatchBusy(false);
    }
  };

  const requestAiSuggestion = async () => {
    if (!selected || !aiInstruction.trim()) return;
    setAiBusy(true);
    setAiSuggestion(null);
    try {
      const res = await aiSuggestPromptTemplate(selected, aiInstruction.trim());
      setAiSuggestion(res.suggestion);
      if (res.suggested_tools && res.suggested_tools.length > 0) {
        setPromptTools((prev) => ({
          assigned_tools: res.suggested_tools!,
          suggested_tools: prev?.suggested_tools ?? [],
          available_tools: prev?.available_tools ?? [],
        }));
        setAiAutoAssigned(res.suggested_tools.length);
      } else {
        setAiAutoAssigned(0);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore generazione AI");
    } finally {
      setAiBusy(false);
    }
  };

  const applyAiSuggestion = () => {
    if (aiSuggestion) {
      setEditContent(aiSuggestion);
      setAiSuggestion(null);
      setAiInstruction("");
      setAiOpen(false);
    }
  };

  const save = async () => {
    if (!selected) return;
    setSaving(true);
    try {
      const updated = await updatePromptTemplate(selected, editContent, changeNote || undefined);
      // Reload detail to get fresh history
      const d = await getPromptTemplate(selected);
      setDetail(d);
      setTemplates((prev) => prev.map((t) => (t.key === selected ? updated : t)));
      setChangeNote("");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore salvataggio");
    } finally {
      setSaving(false);
    }
  };

  const toggleActive = async (key: string, isActive: boolean) => {
    try {
      if (isActive) await disablePromptTemplate(key);
      else await enablePromptTemplate(key);
      setTemplates((prev) => prev.map((t) => (t.key === key ? { ...t, is_active: !isActive } : t)));
      if (selected === key && detail) {
        setDetail({ ...detail, template: { ...detail.template, is_active: !isActive } });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore toggle");
    }
  };

  // La categoria 'profile' è gestita in /admin/profiles — esclusa qui
  const categories = Array.from(new Set(
    templates.filter((t) => t.category !== "profile").map((t) => t.category)
  ));
  const nexusSuggestion = detail?.history.find(
    (h) => h.changed_by === "nexus" && new Date(h.changed_at) > new Date(Date.now() - 86400000),
  );
  const dirty = detail ? editContent !== detail.template.content : false;

  return (
    <div>
      <h1 className="text-3xl font-bold" style={{ marginBottom: 6 }}>Prompt Templates</h1>
      <p className="text-base text-muted" style={{ marginBottom: 12 }}>
        Gestisci i prompt di sistema, le regole di quality scanning e le istruzioni di automazione. Modifiche salvate
        direttamente su DB con versionamento.
      </p>
      <div className="flex-row" style={{ gap: 10, marginBottom: 20 }}>
        <div className="flex-row" style={{ gap: 8 }}>
          <button
            onClick={runBatchAssign}
            disabled={batchBusy}
            className="btn btn-primary text-sm"
            style={{ background: batchBusy ? tc.bgHover : tc.accent, color: batchBusy ? tc.textMuted : "#fff" }}
          >
            {batchBusy ? "Analisi in corso..." : "🤖 Auto-assegna tool"}
          </button>

          <div className="flex-row-gap-4 px-2 py-1" style={{ background: tc.bgCard, borderRadius: 6, border: `1px solid ${tc.border}` }}>
            <span className="text-xs text-muted">Embedding:</span>
            <span className="text-xs font-semibold" style={{ color: "#64c896" }}>🧠 ONNX 384-dim</span>
            <span className="text-xs text-muted">| Cached</span>
          </div>

          <div className="flex-row-gap-4 px-2 py-1" style={{ background: tc.bgCard, borderRadius: 6, border: `1px solid ${tc.border}` }}>
            <span className="text-xs text-muted">Strategie:</span>
            <span className="text-xs">Semantic | Keyword | Lazy</span>
          </div>
        </div>
        {batchResult && (
          <span className="text-sm" style={{ color: batchResult.errors > 0 ? tc.error : "#16a34a" }}>
            ✓ {batchResult.processed} template analizzati — {batchResult.assigned} con tool assegnati{batchResult.errors > 0 ? `, ${batchResult.errors} errori` : ""}
          </span>
        )}

        <button
          onClick={() => setShowAnalytics(!showAnalytics)}
          className="btn text-xs"
          style={{ background: tc.bgHover, border: `1px solid ${tc.border}`, color: tc.text }}
        >
          {showAnalytics ? "📊 Nascondi Analytics" : "📊 Mostra Analytics"}
        </button>
        
        {showAnalytics && (
          <div className="card-sm" style={{ marginTop: 12 }}>
            <div className="text-base font-bold" style={{ marginBottom: 8 }}>🔍 Tool Selection Analytics</div>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
              <div className="text-xs" style={{ padding: 8, background: tc.bgHover, borderRadius: 4 }}>
                <div className="text-muted">Metodo Prevalente</div>
                <div className="text-sm font-semibold" style={{ color: "#64c896", marginTop: 4 }}>🧠 Semantic Search</div>
                <div className="text-xs text-muted" style={{ marginTop: 4 }}>Usa embeddings ONNX 384-dim + cosine similarity</div>
              </div>
              <div className="text-xs" style={{ padding: 8, background: tc.bgHover, borderRadius: 4 }}>
                <div className="text-muted">Fallback Methods</div>
                <div className="text-sm font-semibold" style={{ color: tc.text, marginTop: 4 }}>Keyword (50%) | Lazy Default (50%)</div>
                <div className="text-xs text-muted" style={{ marginTop: 4 }}>Se semantic non trova risultati rilevanti</div>
              </div>
              <div className="text-xs" style={{ padding: 8, background: tc.bgHover, borderRadius: 4 }}>
                <div className="text-muted">Cache Performance</div>
                <div className="text-sm font-semibold" style={{ color: "#64c896", marginTop: 4 }}>~1ms hit | ~5-50ms miss</div>
                <div className="text-xs text-muted" style={{ marginTop: 4 }}>10k entries cached per sessione</div>
              </div>
              <div className="text-xs" style={{ padding: 8, background: tc.bgHover, borderRadius: 4 }}>
                <div className="text-muted">Confidence Threshold</div>
                <div className="text-sm font-semibold" style={{ color: tc.text, marginTop: 4 }}>≥ 30% cosine similarity</div>
                <div className="text-xs text-muted" style={{ marginTop: 4 }}>Tool sotto soglia scartati automaticamente</div>
              </div>
            </div>
          </div>
        )}
      </div>

      {error && (
        <div
          className="text-base"
          style={{
            padding: "10px 14px",
            borderRadius: 6,
            background: `${tc.error}14`,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            marginBottom: 16,
          }}>

          {error}
          <button
            onClick={() => setError(null)}
            className="cursor-pointer text-lg"
            style={{
              float: "right",
              background: "transparent",
              border: "none",
              color: tc.error,
            }}
          >
            ×
          </button>
        </div>
      )}

      <div
        className="card"
        style={{
          display: "grid",
          gridTemplateColumns: "280px 1fr",
          gap: 16,
          minHeight: 500,
          overflow: "hidden",
        }}
      >
        {/* Sidebar list */}
        <div
          className="no-scrollbar overflow-y-auto"
          style={{
            borderRight: `1px solid ${tc.border}`,
            padding: 12,
          }}
        >
          {loading ? (
            <div className="text-base text-muted" style={{ padding: 12 }}>Caricamento...</div>
          ) : categories.length === 0 ? (
            <div className="text-base text-muted" style={{ padding: 12 }}>Nessun template</div>
          ) : (
            categories.map((cat) => {
              const isExpanded = expandedCategories.has(cat);
              return (
              <div key={cat} style={{ marginBottom: 4 }}>
                <div
                  onClick={() => toggleCategory(cat)}
                  className="flex-row cursor-pointer text-xs font-semibold text-muted"
                  style={{
                    justifyContent: "space-between",
                    textTransform: "uppercase",
                    letterSpacing: 0.5,
                    marginBottom: isExpanded ? 6 : 0,
                    padding: "4px 8px",
                    borderRadius: 4,
                    userSelect: "none",
                  }}
                  onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.bgHover; }}
                  onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
                >
                  <span>{cat}</span>
                  <span style={{ fontSize: 10, opacity: 0.7 }}>{isExpanded ? "▾" : "▸"}</span>
                </div>
                {isExpanded && templates
                  .filter((t) => t.category === cat)
                  .map((t) => {
                    const isActive = selected === t.key;
                    return (
                      <div
                        key={t.key}
                        onClick={() => selectTemplate(t.key)}
                        style={{
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                          padding: "8px 10px",
                          borderRadius: 6,
                          cursor: "pointer",
                          fontSize: 13,
                          background: isActive ? tc.bgHover : "transparent",
                          color: t.is_active ? tc.text : tc.textMuted,
                          textDecoration: t.is_active ? "none" : "line-through",
                          marginBottom: 2,
                        }}
                        onMouseEnter={(e) => {
                          if (!isActive) (e.currentTarget as HTMLDivElement).style.background = tc.bgHover;
                        }}
                        onMouseLeave={(e) => {
                          if (!isActive) (e.currentTarget as HTMLDivElement).style.background = "transparent";
                        }}
                      >
                        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                          {t.title}
                        </span>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            toggleActive(t.key, t.is_active);
                          }}
                          title={t.is_active ? "Disabilita regola" : "Abilita regola"}
                          style={{
                            background: "transparent",
                            border: "none",
                            cursor: "pointer",
                            fontSize: 14,
                            color: t.is_active ? "#22c55e" : tc.textMuted,
                            marginLeft: 8,
                            padding: 0,
                          }}
                        >
                          {t.is_active ? "●" : "○"}
                        </button>
                      </div>
                    );
                  })}
              </div>
            );
            })
          )}
        </div>

        {/* Editor */}
        <div className="flex-col" style={{ padding: 20, gap: 12, minWidth: 0 }}>
          {!detail ? (
            <div
              className="flex-row text-base text-muted"
              style={{
                justifyContent: "center",
                height: "100%",
              }}
            >
              Seleziona un template dalla lista
            </div>
          ) : (
            <>
              <div className="flex-row" style={{ justifyContent: "space-between", gap: 12 }}>
                <div style={{ minWidth: 0 }}>
                  <h3 className="text-lg font-bold" style={{ margin: 0 }}>
                    {detail.template.title}
                  </h3>
                  <div className="text-xs text-muted" style={{ marginTop: 4 }}>
                    <code>{detail.template.key}</code> · v{detail.template.version} ·{" "}
                    {detail.template.updated_by} · {new Date(detail.template.updated_at).toLocaleString("it-IT")}
                  </div>
                </div>
                <div style={{ display: "flex", gap: 6, flexShrink: 0, alignItems: "center" }}>
                  {detail.template.schema_type === "xml" && (
                    <span
                      title="Prompt nel nuovo schema XML v2 (con placeholder runtime e tag <role>, <autonomia>, ecc.)"
                      style={{
                        fontSize: 10,
                        fontWeight: 700,
                        padding: "3px 7px",
                        borderRadius: 4,
                        background: `${tc.accent}14`,
                        color: tc.accent,
                        whiteSpace: "nowrap",
                        letterSpacing: "0.04em",
                      }}
                    >
                      SCHEMA XML v2
                    </span>
                  )}
                  {detail.template.experimental && (
                    <span
                      title="Variante sperimentale generata dal PromptOptimizerWorker (canary A/B)"
                      style={{
                        fontSize: 10,
                        fontWeight: 700,
                        padding: "3px 7px",
                        borderRadius: 4,
                        background: "#f59e0b22",
                        color: "#b45309",
                        whiteSpace: "nowrap",
                        letterSpacing: "0.04em",
                      }}
                    >
                      SPERIMENTALE
                    </span>
                  )}
                  {!detail.template.is_active && (
                    <span
                      style={{
                        fontSize: 11,
                        fontWeight: 600,
                        padding: "3px 8px",
                        borderRadius: 4,
                        background: `${tc.error}14`,
                        color: tc.error,
                        whiteSpace: "nowrap",
                      }}
                    >
                      Disabilitata
                    </span>
                  )}
                </div>
              </div>

              {nexusSuggestion && (
                <div
                  style={{
                    border: `1px solid ${tc.accent}`,
                    background: tc.accentBg,
                    borderRadius: 6,
                    padding: 10,
                    fontSize: 12,
                  }}
                >
                  <div style={{ fontWeight: 600, color: tc.accent, marginBottom: 4 }}>
                    Nexus suggerisce una modifica
                  </div>
                  <div style={{ color: tc.textMuted, marginBottom: 8 }}>{nexusSuggestion.change_note}</div>
                  <button
                    onClick={() => setEditContent(nexusSuggestion.content)}
                    style={{
                      fontSize: 11,
                      padding: "4px 10px",
                      background: tc.accent,
                      color: "#fff",
                      border: "none",
                      borderRadius: 4,
                      cursor: "pointer",
                    }}
                  >
                    Applica suggerimento
                  </button>
                </div>
              )}

              {detail.template.usage_context && (
                <details
                  style={{
                    fontSize: 12,
                    border: `1px solid ${tc.border}`,
                    borderRadius: 6,
                    padding: "8px 10px",
                    background: tc.bg,
                  }}
                >
                  <summary style={{ cursor: "pointer", color: tc.textMuted, fontWeight: 600 }}>
                    Contesto d&apos;uso (dove viene usato questo prompt)
                  </summary>
                  <div
                    style={{
                      marginTop: 8,
                      color: tc.text,
                      fontSize: 12,
                      lineHeight: 1.5,
                      whiteSpace: "pre-wrap",
                    }}
                  >
                    {detail.template.usage_context}
                  </div>
                </details>
              )}

              {/* Widget Anteprima resa: mostra il prompt con i placeholder
                  ({{lang_hint}}, {{type_hint}}, {{repo_summary}}) sostituiti
                  con i valori scelti dall'admin. Sempre presente; per i prompt
                  che non hanno placeholder mostra il contenuto identico al raw. */}
              <details
                style={{
                  fontSize: 12,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 6,
                  padding: "8px 10px",
                  background: tc.bg,
                }}
              >
                <summary style={{ cursor: "pointer", color: tc.textMuted, fontWeight: 600 }}>
                  Anteprima resa (sostituzione placeholder runtime)
                  {detail.template.placeholder_vars && detail.template.placeholder_vars.length > 0 && (
                    <span style={{ marginLeft: 8, color: tc.accent, fontWeight: 500 }}>
                      [{detail.template.placeholder_vars.join(", ")}]
                    </span>
                  )}
                </summary>
                <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 8 }}>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                    <label style={{ fontSize: 11, color: tc.textMuted }}>
                      Intent:&nbsp;
                      <select
                        value={previewIntent}
                        onChange={(e) => setPreviewIntent(e.target.value)}
                        style={{
                          fontSize: 11,
                          padding: "3px 6px",
                          background: tc.bg,
                          color: tc.text,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                        }}
                      >
                        {[
                          "chat", "code_generation", "code_modification", "bug_fix",
                          "refactoring", "test_generation", "code_review",
                          "documentation", "architecture", "performance",
                          "security", "database", "infrastructure", "deployment",
                        ].map((i) => (
                          <option key={i} value={i}>{i}</option>
                        ))}
                      </select>
                    </label>
                    <label style={{ fontSize: 11, color: tc.textMuted }}>
                      Linguaggio:&nbsp;
                      <input
                        value={previewLang}
                        onChange={(e) => setPreviewLang(e.target.value)}
                        placeholder="es. TypeScript"
                        style={{
                          fontSize: 11,
                          padding: "3px 6px",
                          background: tc.bg,
                          color: tc.text,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                          width: 120,
                        }}
                      />
                    </label>
                    <button
                      onClick={() => detail && runPreview(detail.template.key)}
                      disabled={previewLoading}
                      style={{
                        fontSize: 11,
                        padding: "4px 10px",
                        background: tc.accent,
                        color: "#fff",
                        border: "none",
                        borderRadius: 4,
                        cursor: previewLoading ? "wait" : "pointer",
                        opacity: previewLoading ? 0.6 : 1,
                      }}
                    >
                      {previewLoading ? "Rendering..." : "Genera anteprima"}
                    </button>
                  </div>
                  {preview && (
                    <>
                      {preview.unresolved_placeholders.length > 0 && (
                        <div
                          style={{
                            fontSize: 11,
                            padding: "6px 8px",
                            background: "#f59e0b14",
                            color: "#b45309",
                            borderRadius: 4,
                          }}
                        >
                          Placeholder non risolti (sostituiti con stringa vuota):{" "}
                          <code>{preview.unresolved_placeholders.join(", ")}</code>
                        </div>
                      )}
                      <pre
                        style={{
                          margin: 0,
                          padding: "8px 10px",
                          background: tc.bg,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                          fontSize: 11,
                          lineHeight: 1.45,
                          color: tc.text,
                          whiteSpace: "pre-wrap",
                          maxHeight: 320,
                          overflow: "auto",
                        }}
                      >
                        {preview.rendered}
                      </pre>
                    </>
                  )}
                </div>
              </details>

              <div style={{ position: "relative", display: "flex", flexDirection: "column" }}>
                <textarea
                  value={editContent}
                  onChange={(e) => setEditContent(e.target.value)}
                  placeholder="Contenuto del prompt..."
                  style={{
                    width: "100%",
                    minHeight: 480,
                    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                    fontSize: 13,
                    lineHeight: 1.55,
                    border: `2px solid ${tc.border}`,
                    borderRadius: 8,
                    padding: 14,
                    background: tc.bg,
                    color: tc.text,
                    resize: "both",
                    overflow: "auto",
                    boxSizing: "border-box",
                  }}
                />
                <div style={{ fontSize: 10, color: tc.textMuted, marginTop: 4, alignSelf: "flex-end" }}>
                  Trascina l&apos;angolo in basso a destra per ridimensionare ↘
                </div>
              </div>

              {/* AI Assistant panel */}
              <div
                style={{
                  border: `1px solid ${aiOpen ? tc.accent : tc.border}`,
                  borderRadius: 8,
                  padding: 12,
                  background: aiOpen ? `${tc.accent}08` : tc.bg,
                  transition: "all 0.15s",
                }}
              >
                <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
                  <div style={{ fontSize: 13, fontWeight: 600, color: tc.text, display: "flex", alignItems: "center", gap: 6 }}>
                    <span>✨ Aiuto AI</span>
                    <span style={{ fontSize: 11, color: tc.textMuted, fontWeight: 400 }}>
                      Riscrivi il prompt con l&apos;assistenza di un modello (riceve il contesto d&apos;uso pre-inserito)
                    </span>
                  </div>
                  <button
                    onClick={() => setAiOpen((v) => !v)}
                    style={{
                      padding: "4px 12px",
                      background: aiOpen ? tc.bgHover : tc.accent,
                      color: aiOpen ? tc.text : "#fff",
                      border: "none",
                      borderRadius: 6,
                      fontSize: 12,
                      fontWeight: 600,
                      cursor: "pointer",
                    }}
                  >
                    {aiOpen ? "Chiudi" : "Apri"}
                  </button>
                </div>

                {aiOpen && (
                  <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 10 }}>
                    <textarea
                      value={aiInstruction}
                      onChange={(e) => setAiInstruction(e.target.value)}
                      placeholder={`Cosa vuoi modificare? Es: "rendilo più conciso", "aggiungi una regola contro l'uso di console.log", "traduci in inglese", "aggiungi gestione async/await"...`}
                      style={{
                        width: "100%",
                        minHeight: 80,
                        fontSize: 12,
                        fontFamily: "inherit",
                        lineHeight: 1.5,
                        border: `1px solid ${tc.border}`,
                        borderRadius: 6,
                        padding: 10,
                        background: tc.bg,
                        color: tc.text,
                        resize: "vertical",
                        boxSizing: "border-box",
                      }}
                    />
                    <div style={{ display: "flex", gap: 8 }}>
                      <button
                        onClick={requestAiSuggestion}
                        disabled={aiBusy || !aiInstruction.trim()}
                        style={{
                          padding: "6px 14px",
                          background: !aiBusy && aiInstruction.trim() ? tc.accent : tc.bgHover,
                          color: !aiBusy && aiInstruction.trim() ? "#fff" : tc.textMuted,
                          border: "none",
                          borderRadius: 6,
                          fontSize: 12,
                          fontWeight: 600,
                          cursor: !aiBusy && aiInstruction.trim() ? "pointer" : "default",
                        }}
                      >
                        {aiBusy ? "Generando..." : "Genera suggerimento"}
                      </button>
                      {aiSuggestion && (
                        <button
                          onClick={applyAiSuggestion}
                          style={{
                            padding: "6px 14px",
                            background: "#22c55e",
                            color: "#fff",
                            border: "none",
                            borderRadius: 6,
                            fontSize: 12,
                            fontWeight: 600,
                            cursor: "pointer",
                          }}
                        >
                          Sostituisci nel prompt
                        </button>
                      )}
                    </div>
                    {aiSuggestion && (
                      <div
                        style={{
                          border: `1px solid ${tc.border}`,
                          borderRadius: 6,
                          padding: 10,
                          background: tc.bgCard,
                          maxHeight: 320,
                          overflow: "auto",
                          fontSize: 12,
                          fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                          lineHeight: 1.5,
                          color: tc.text,
                          whiteSpace: "pre-wrap",
                        }}
                      >
                        {aiSuggestion}
                      </div>
                    )}
                    {aiAutoAssigned > 0 && (
                      <div style={{ display: "flex", alignItems: "center", gap: 6, padding: "6px 10px", background: "#22c55e18", border: "1px solid #22c55e44", borderRadius: 6, fontSize: 11, color: "#16a34a" }}>
                        <span>&#10003;</span>
                        <span><strong>{aiAutoAssigned} tool MCP</strong> assegnati automaticamente a questo template</span>
                      </div>
                    )}
                  </div>
                )}
              </div>

              {/* MCP Tools Section */}
              <div
                style={{
                  border: `1px solid ${toolsOpen ? tc.accent : tc.border}`,
                  borderRadius: 8,
                  padding: 12,
                  background: toolsOpen ? `${tc.accent}08` : tc.bg,
                  transition: "all 0.15s",
                }}
              >
                <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
                  <div style={{ fontSize: 13, fontWeight: 600, color: tc.text, display: "flex", alignItems: "center", gap: 6 }}>
                    <span>🔧 MCP Tools</span>
                    {promptTools && promptTools.assigned_tools.length > 0 && (
                      <span style={{ fontSize: 11, color: tc.accent, fontWeight: 600, background: `${tc.accent}20`, padding: "2px 6px", borderRadius: 3 }}>
                        {promptTools.assigned_tools.length}
                      </span>
                    )}
                  </div>
                  <button
                    onClick={() => {
                      setToolsOpen(!toolsOpen);
                      if (!toolsOpen) loadAvailableTools();
                    }}
                    style={{
                      padding: "4px 12px",
                      background: toolsOpen ? tc.bgHover : tc.accent,
                      color: toolsOpen ? tc.text : "#fff",
                      border: "none",
                      borderRadius: 6,
                      fontSize: 12,
                      fontWeight: 600,
                      cursor: "pointer",
                    }}
                  >
                    {toolsOpen ? "Chiudi" : "Configura"}
                  </button>
                </div>

                {toolsOpen && promptTools && (
                  <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 10 }}>
                    <div>
                      <div style={{ fontSize: 12, fontWeight: 600, color: tc.text, marginBottom: 8 }}>
                        Tool assegnati ({promptTools.assigned_tools.length})
                      </div>
                      {promptTools.assigned_tools.length === 0 ? (
                        <div style={{ fontSize: 12, color: tc.textMuted, padding: "8px 10px", background: tc.bg, borderRadius: 4 }}>
                          Nessun tool assegnato. Selezionane uno dalla lista disponibile
                        </div>
                      ) : (
                        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                          {promptTools.assigned_tools.map((tool, idx) => (
                            <div
                              key={idx}
                              style={{
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "space-between",
                                padding: "8px 10px",
                                background: tc.bgCard,
                                borderRadius: 4,
                                border: `1px solid ${tc.border}`,
                              }}
                            >
                              <div>
                                <div style={{ fontSize: 12, fontWeight: 600, color: tc.text }}>
                                  {tool.tool_name}
                                </div>
                                <div style={{ fontSize: 11, color: tc.textMuted }}>
                                  {tool.tool_server}
                                </div>
                                {(tool.confidence || tool.method) && (
                                  <div style={{ fontSize: 10, marginTop: 4, color: tc.textMuted, display: "flex", gap: 8 }}>
                                    {tool.confidence !== undefined && (
                                      <span style={{ background: `rgba(100, 200, 150, ${Math.max(0.2, tool.confidence)})`, padding: "2px 6px", borderRadius: 3 }}>
                                        {Math.round(tool.confidence * 100)}% confidence
                                      </span>
                                    )}
                                    {tool.method && (
                                      <span style={{ background: tc.bgHover, padding: "2px 6px", borderRadius: 3 }}>
                                        {tool.method === "semantic" ? "🧠 Semantic" : tool.method === "keyword" ? "🔍 Keyword" : "📋 Default"}
                                      </span>
                                    )}
                                  </div>
                                )}
                              </div>
                              <button
                                onClick={() => toggleToolSelection(tool)}
                                style={{
                                  background: tc.error,
                                  color: "#fff",
                                  border: "none",
                                  borderRadius: 4,
                                  padding: "4px 8px",
                                  fontSize: 11,
                                  cursor: "pointer",
                                }}
                              >
                                Rimuovi
                              </button>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>

                    <details style={{ fontSize: 12 }}>
                      <summary style={{ cursor: "pointer", color: tc.textMuted, fontWeight: 600, marginBottom: 8 }}>
                        Tool disponibili ({availableTools.length})
                      </summary>
                      {toolsLoading ? (
                        <div style={{ color: tc.textMuted, padding: "8px 10px" }}>Caricamento tools...</div>
                      ) : availableTools.length === 0 ? (
                        <div style={{ color: tc.textMuted, padding: "8px 10px" }}>Nessun MCP tool installato</div>
                      ) : (
                        <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 8, maxHeight: 300, overflowY: "auto" }}>
                          {availableTools.map((tool, idx) => {
                            const isSelected = promptTools.assigned_tools.some(
                              t => t.tool_name === tool.name && t.tool_server === tool.server
                            );
                            return (
                              <button
                                key={idx}
                                onClick={() => toggleToolSelection({ tool_name: tool.name, tool_server: tool.server, usage_context: tool.description })}
                                style={{
                                  padding: "8px 10px",
                                  background: isSelected ? tc.accent : tc.bg,
                                  color: isSelected ? "#fff" : tc.text,
                                  border: `1px solid ${isSelected ? tc.accent : tc.border}`,
                                  borderRadius: 4,
                                  fontSize: 12,
                                  cursor: "pointer",
                                  textAlign: "left",
                                  transition: "all 0.15s",
                                }}
                                onMouseEnter={(e) => {
                                  if (!isSelected) (e.currentTarget as HTMLButtonElement).style.background = tc.bgHover;
                                }}
                                onMouseLeave={(e) => {
                                  if (!isSelected) (e.currentTarget as HTMLButtonElement).style.background = tc.bg;
                                }}
                              >
                                <div style={{ fontSize: 12, fontWeight: 600 }}>
                                  {tool.name} {isSelected && "✓"}
                                </div>
                                <div style={{ fontSize: 11, color: isSelected ? "#fff7" : tc.textMuted }}>
                                  {tool.server}
                                </div>
                              </button>
                            );
                          })}
                        </div>
                      )}
                    </details>

                    <button
                      onClick={saveTools}
                      style={{
                        padding: "6px 12px",
                        background: tc.accent,
                        color: "#fff",
                        border: "none",
                        borderRadius: 6,
                        fontSize: 12,
                        fontWeight: 600,
                        cursor: "pointer",
                      }}
                    >
                      Salva Tools
                    </button>
                  </div>
                )}
              </div>

              <div className="flex-row" style={{ gap: 8 }}>
                <textarea
                  value={changeNote}
                  onChange={(e) => setChangeNote(e.target.value)}
                  placeholder="Nota modifica (opzionale)"
                  rows={2}
                  style={{
                    flex: 1,
                    fontSize: 12,
                    padding: "8px 10px",
                    border: `1px solid ${tc.border}`,
                    borderRadius: 6,
                    background: tc.bg,
                    color: tc.text,
                    resize: "vertical",
                    fontFamily: "inherit",
                    boxSizing: "border-box",
                  }}
                />
                <button
                  onClick={save}
                  disabled={saving || !dirty}
                  style={{
                    padding: "8px 20px",
                    background: dirty && !saving ? tc.accent : tc.bgHover,
                    color: dirty && !saving ? "#fff" : tc.textMuted,
                    border: "none",
                    borderRadius: 6,
                    fontSize: 13,
                    fontWeight: 600,
                    cursor: dirty && !saving ? "pointer" : "default",
                    whiteSpace: "nowrap",
                  }}
                >
                  {saving ? "Salvataggio..." : "Salva"}
                </button>
              </div>

              {detail.history.length > 0 && (
                <details style={{ fontSize: 12 }}>
                  <summary style={{ cursor: "pointer", color: tc.textMuted }}>
                    Storico versioni ({detail.history.length})
                  </summary>
                  <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 4, maxHeight: 200, overflowY: "auto" }}>
                    {detail.history.map((h) => (
                      <div
                        key={h.id}
                        style={{
                          display: "flex",
                          gap: 8,
                          padding: "6px 8px",
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                          fontSize: 11,
                          alignItems: "center",
                        }}
                      >
                        <span style={{ color: tc.textMuted, fontWeight: 600 }}>v{h.version}</span>
                        <span
                          style={{
                            color:
                              h.changed_by === "nexus"
                                ? tc.accent
                                : h.changed_by === "user"
                                  ? "#16a34a"
                                  : tc.textMuted,
                            fontWeight: 600,
                          }}
                        >
                          {h.changed_by}
                        </span>
                        <span style={{ color: tc.textMuted }}>
                          {new Date(h.changed_at).toLocaleString("it-IT")}
                        </span>
                        {h.change_note && (
                          <span style={{ color: tc.textMuted, fontStyle: "italic" }}>{h.change_note}</span>
                        )}
                      </div>
                    ))}
                  </div>
                </details>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
