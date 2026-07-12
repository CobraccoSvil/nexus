"use client";

/**
 * Pagina admin: gestione del Consiglio delle Competenze (advisory panel).
 *
 * Vista curata delle figure advisory (mig 0546/0553/0554) con:
 *  - stato del consiglio (toggle council_enabled + parametri read-only)
 *  - tabella consiglieri (definizione, modello, warning di misconfigurazione,
 *    editor del prompt con storico versionato, toggle abilitazione)
 *  - composizione dei gruppi Base/Infra (CSV orchestrator.council_figures /
 *    council_infra_figures)
 *
 * Nessun endpoint nuovo: definizioni via admin-service
 * (/api/admin/orchestrator/subagents/*, rewrite Next -> :4010), prompt via
 * mcp-core (/api/prompt-templates/:key, history + version bump), settings via
 * /api/admin/setting/:key (punti unici esistenti, regola L). Il roster
 * effettivo per run resta deciso dal pre-step convene_council in mcp-core:
 * questa pagina mostra la composizione, non simula l'attivazione.
 */

import { Fragment, useCallback, useMemo, useState, type CSSProperties, type ReactNode } from "react";

import { AdminModal } from "../../../components/admin/AdminModal";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";
import { useGlobalDialog } from "../../../components/global-dialog-provider";
import {
  listSubagentDefinitions,
  toSubagentUpsertBody,
  upsertSubagentDefinition,
  type SubagentDefinition,
} from "../../../lib/api/agent";
import {
  listAdminPurposeModels,
  listAdminSettingsByCategory,
  updateAdminSetting,
  type AdminSettingEntry,
  type PurposeModelEntry,
} from "../../../lib/api/admin-settings";
import {
  getPromptTemplate,
  updatePromptTemplate,
  type PromptTemplate,
  type PromptTemplateHistory,
} from "../../../lib/api/prompts";
import { useThemeColors } from "../../../lib/theme";
import { useListData } from "../../../lib/use-list-data";

type ThemeColors = ReturnType<typeof useThemeColors>;

// ── Derivazione roster e config (logica pura, presentazione dei settings) ────

type CouncilGroup = "base" | "infra" | "multi_provider";

const GROUP_LABEL: Record<CouncilGroup, string> = {
  base: "Base",
  infra: "Infrastruttura",
  multi_provider: "Multi-provider",
};

interface CouncilMember {
  kind: string;
  group: CouncilGroup;
  /** null = kind elencato nei settings ma assente in nexus_subagent_definitions. */
  definition: SubagentDefinition | null;
  /** Guard 1 del dispatcher: fuori da orchestrator.subagent_kinds_whitelist il kind non e' convocabile. */
  inWhitelist: boolean;
  /** Un consigliere senza advisory_verdict in whitelist non puo' emettere il verdetto strutturato. */
  hasAdvisoryVerdict: boolean;
  purposeModel: PurposeModelEntry | null;
}

interface CouncilConfig {
  enabled: boolean;
  maxFigures: number | null;
  minValidVerdicts: number | null;
  blockOnHighSeverity: boolean;
  minTriggerHits: number | null;
  multiProviderEnabled: boolean;
  multiProviderKind: string | null;
  baseFigures: string[];
  infraFigures: string[];
  whitelist: string[];
}

function parseCsv(value: string | null): string[] {
  if (!value) return [];
  return value
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

function parseNum(value: string | null): number | null {
  if (value === null || value.trim() === "") return null;
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

function deriveCouncil(
  settings: AdminSettingEntry[],
  definitions: SubagentDefinition[],
  purposes: PurposeModelEntry[],
): { members: CouncilMember[]; config: CouncilConfig } {
  const get = (suffix: string): string | null =>
    settings.find((s) => s.key === `orchestrator.${suffix}`)?.value ?? null;

  const baseFigures = parseCsv(get("council_figures"));
  const infraFigures = parseCsv(get("council_infra_figures"));
  const multiProviderKind = (get("multi_provider_kind") ?? "").trim() || null;
  const whitelist = parseCsv(get("subagent_kinds_whitelist"));

  const config: CouncilConfig = {
    enabled: get("council_enabled") === "true",
    maxFigures: parseNum(get("council_max_figures")),
    minValidVerdicts: parseNum(get("council_advisory_min_valid")),
    blockOnHighSeverity: get("council_advisory_block_on_high_severity") === "true",
    minTriggerHits: parseNum(get("council_min_trigger_hits")),
    multiProviderEnabled: get("multi_provider_enabled") === "true",
    multiProviderKind,
    baseFigures,
    infraFigures,
    whitelist,
  };

  const defByKind = new Map(definitions.map((d) => [d.kind, d]));
  const purposeByKey = new Map(purposes.map((p) => [p.purpose, p]));
  const seen = new Set<string>();
  const members: CouncilMember[] = [];
  // Dedup con precedenza base > infra > multi-provider.
  const push = (kind: string, group: CouncilGroup) => {
    if (seen.has(kind)) return;
    seen.add(kind);
    const definition = defByKind.get(kind) ?? null;
    members.push({
      kind,
      group,
      definition,
      inWhitelist: whitelist.includes(kind),
      hasAdvisoryVerdict: definition ? definition.toolWhitelist.includes("advisory_verdict") : false,
      purposeModel: definition ? (purposeByKey.get(definition.modelPurpose) ?? null) : null,
    });
  };
  baseFigures.forEach((k) => push(k, "base"));
  infraFigures.forEach((k) => push(k, "infra"));
  if (multiProviderKind) push(multiProviderKind, "multi_provider");

  return { members, config };
}

function formatDate(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("it-IT", {
    day: "2-digit",
    month: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// ── Stato modali ──────────────────────────────────────────────────────────────

interface PromptEditorState {
  kind: string;
  promptKey: string;
  loading: boolean;
  loadError: string | null;
  template: PromptTemplate | null;
  history: PromptTemplateHistory[];
  content: string;
  changeNote: string;
  saving: boolean;
  saveError: string | null;
  expandedHistoryId: string | null;
}

interface DescEditorState {
  def: SubagentDefinition;
  text: string;
  saving: boolean;
  error: string | null;
}

// ── Pagina ────────────────────────────────────────────────────────────────────

export default function CouncilPage() {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();

  const defs = useListData<SubagentDefinition>(
    useCallback(() => listSubagentDefinitions().then((r) => r.definitions), []),
  );
  const settings = useListData<AdminSettingEntry>(
    useCallback(() => listAdminSettingsByCategory("orchestrator").then((r) => r.settings), []),
  );
  const purposes = useListData<PurposeModelEntry>(
    useCallback(() => listAdminPurposeModels().then((r) => r.items), []),
  );

  const { members, config } = useMemo(
    () => deriveCouncil(settings.data, defs.data, purposes.data),
    [settings.data, defs.data, purposes.data],
  );

  const reloadDefs = defs.reload;
  const reloadSettings = settings.reload;
  const reloadPurposes = purposes.reload;
  const reloadAll = useCallback(() => {
    void reloadDefs();
    void reloadSettings();
    void reloadPurposes();
  }, [reloadDefs, reloadSettings, reloadPurposes]);

  // Sezione A: toggle consiglio.
  const [councilBusy, setCouncilBusy] = useState(false);
  const [councilError, setCouncilError] = useState<string | null>(null);

  const toggleCouncil = async () => {
    setCouncilBusy(true);
    setCouncilError(null);
    try {
      await updateAdminSetting("orchestrator.council_enabled", config.enabled ? "false" : "true");
      await reloadSettings();
    } catch (e) {
      setCouncilError(e instanceof Error ? e.message : "Errore salvataggio setting");
    } finally {
      setCouncilBusy(false);
    }
  };

  // Sezione B: azioni per riga.
  const [rowBusy, setRowBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);
  const [promptEditor, setPromptEditor] = useState<PromptEditorState | null>(null);
  const [descEditor, setDescEditor] = useState<DescEditorState | null>(null);

  const toggleEnabled = async (member: CouncilMember) => {
    const def = member.definition;
    if (!def) return;
    if (def.isEnabled) {
      const ok = await confirmDialog({
        title: "Disabilita consigliere",
        message: `Disabilitare '${def.kind}'? La figura non sara' piu' convocabile dal dispatcher.`,
        danger: true,
        confirmLabel: "Disabilita",
        cancelLabel: "Annulla",
      });
      if (!ok) return;
    }
    setRowBusy(def.kind);
    setRowError(null);
    try {
      // Upsert FULL-BODY (ON CONFLICT aggiorna tutte le colonne): si rispedisce
      // l'intera definizione col solo flag invertito. Niente optimistic update.
      await upsertSubagentDefinition({ ...toSubagentUpsertBody(def), is_enabled: !def.isEnabled });
      await reloadDefs();
    } catch (e) {
      setRowError(e instanceof Error ? e.message : "Errore aggiornamento definizione");
    } finally {
      setRowBusy(null);
    }
  };

  const saveDescription = async () => {
    if (!descEditor) return;
    setDescEditor((s) => s && { ...s, saving: true, error: null });
    try {
      await upsertSubagentDefinition({
        ...toSubagentUpsertBody(descEditor.def),
        description: descEditor.text.trim() || null,
      });
      setDescEditor(null);
      await reloadDefs();
    } catch (e) {
      setDescEditor(
        (s) => s && { ...s, saving: false, error: e instanceof Error ? e.message : "Errore salvataggio" },
      );
    }
  };

  // Editor prompt.
  const openPromptEditor = async (member: CouncilMember) => {
    const def = member.definition;
    if (!def) return;
    const key = def.promptKey;
    setPromptEditor({
      kind: def.kind,
      promptKey: key,
      loading: true,
      loadError: null,
      template: null,
      history: [],
      content: "",
      changeNote: "",
      saving: false,
      saveError: null,
      expandedHistoryId: null,
    });
    try {
      const r = await getPromptTemplate(key);
      setPromptEditor((p) =>
        p && p.promptKey === key
          ? {
              ...p,
              loading: false,
              template: r.template,
              history: r.history,
              content: r.template.content,
            }
          : p,
      );
    } catch (e) {
      setPromptEditor((p) =>
        p && p.promptKey === key
          ? {
              ...p,
              loading: false,
              loadError: e instanceof Error ? e.message : "Errore caricamento template",
            }
          : p,
      );
    }
  };

  const promptDirty = Boolean(
    promptEditor && promptEditor.template && promptEditor.content !== promptEditor.template.content,
  );

  const closePromptEditor = async () => {
    if (promptDirty) {
      const ok = await confirmDialog({
        title: "Modifiche non salvate",
        message: "Il prompt contiene modifiche non salvate. Chiudere senza salvare?",
        danger: true,
        confirmLabel: "Chiudi senza salvare",
        cancelLabel: "Torna all'editor",
      });
      if (!ok) return;
    }
    setPromptEditor(null);
  };

  const savePrompt = async () => {
    if (!promptEditor || !promptEditor.template || promptEditor.saving) return;
    if (!promptEditor.content.trim() || !promptDirty) return;
    const key = promptEditor.promptKey;
    setPromptEditor((p) => p && { ...p, saving: true, saveError: null });
    try {
      await updatePromptTemplate(key, promptEditor.content, promptEditor.changeNote.trim() || undefined);
      // Re-fetch: history fresca e version aggiornata dal backend.
      const r = await getPromptTemplate(key);
      setPromptEditor((p) =>
        p && p.promptKey === key
          ? {
              ...p,
              saving: false,
              template: r.template,
              history: r.history,
              content: r.template.content,
              changeNote: "",
            }
          : p,
      );
    } catch (e) {
      setPromptEditor((p) =>
        p
          ? {
              ...p,
              saving: false,
              saveError: e instanceof Error ? e.message : "Errore salvataggio prompt",
            }
          : p,
      );
    }
  };

  // Sezione C: composizione (draft locale, salvataggio via punto unico settings).
  const [draftBase, setDraftBase] = useState<string[] | null>(null);
  const [draftInfra, setDraftInfra] = useState<string[] | null>(null);
  const [compSaving, setCompSaving] = useState(false);
  const [compError, setCompError] = useState<string | null>(null);

  const effBase = draftBase ?? config.baseFigures;
  const effInfra = draftInfra ?? config.infraFigures;
  const compDirty = draftBase !== null || draftInfra !== null;

  const toggleComposition = (kind: string, group: "base" | "infra") => {
    const current = group === "base" ? effBase : effInfra;
    const next = current.includes(kind) ? current.filter((k) => k !== kind) : [...current, kind];
    if (group === "base") setDraftBase(next);
    else setDraftInfra(next);
  };

  const saveComposition = async () => {
    setCompSaving(true);
    setCompError(null);
    try {
      if (draftBase !== null) {
        await updateAdminSetting("orchestrator.council_figures", draftBase.join(","));
      }
      if (draftInfra !== null) {
        await updateAdminSetting("orchestrator.council_infra_figures", draftInfra.join(","));
      }
      setDraftBase(null);
      setDraftInfra(null);
      await reloadSettings();
    } catch (e) {
      setCompError(e instanceof Error ? e.message : "Errore salvataggio composizione");
    } finally {
      setCompSaving(false);
    }
  };

  const candidates = useMemo(
    () => [...defs.data].sort((a, b) => a.kind.localeCompare(b.kind)),
    [defs.data],
  );

  const rosterLoading = settings.loading || defs.loading;
  const rosterError = settings.error ?? defs.error;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 28, minWidth: 0 }}>
      <AdminPageHeader
        title="Consiglio delle Competenze"
        description="Figure advisory read-only convocate prima dell'esecuzione dei task sensibili. Da qui si modificano i prompt che le descrivono (con versionamento), l'abilitazione e la composizione dei gruppi."
        action={
          <button type="button" onClick={reloadAll} style={btnStyle(tc, "ghost")}>
            Ricarica
          </button>
        }
      />

      {/* ── Sezione A: stato del consiglio ─────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Stato del consiglio"
        subtitle="Attivazione e parametri di quorum. Il roster effettivo di ogni run e' deciso dal pre-step convene_council in base al contenuto del task."
        loading={settings.loading}
        error={settings.error}
        empty={false}
        emptyLabel=""
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
            <Badge tc={tc} tone={config.enabled ? "ok" : "muted"} label={config.enabled ? "Consiglio attivo" : "Consiglio disattivato"} />
            <button
              type="button"
              onClick={() => void toggleCouncil()}
              disabled={councilBusy}
              style={btnStyle(tc, config.enabled ? "danger" : "primary")}
            >
              {councilBusy ? "Salvataggio…" : config.enabled ? "Disattiva consiglio" : "Attiva consiglio"}
            </button>
            {councilError ? <span style={{ fontSize: 12, color: tc.error }}>{councilError}</span> : null}
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <ConfigChip tc={tc} label="Max figure" value={config.maxFigures?.toString() ?? "—"} />
            <ConfigChip tc={tc} label="Quorum verdetti validi" value={config.minValidVerdicts?.toString() ?? "—"} />
            <ConfigChip tc={tc} label="Veto su severita' alta" value={config.blockOnHighSeverity ? "attivo" : "no"} />
            <ConfigChip tc={tc} label="Trigger hits minimi" value={config.minTriggerHits?.toString() ?? "—"} />
            <ConfigChip tc={tc} label="Panel multi-provider" value={config.multiProviderEnabled ? "attivo" : "no"} />
          </div>
          <div style={{ fontSize: 12, color: tc.textMuted }}>
            Parametri avanzati (keyword di attivazione, quorum, whitelist dispatcher):{" "}
            <a href="/admin/settings/orchestrator" style={{ color: tc.accent }}>
              Impostazioni orchestrator
            </a>
          </div>
        </div>
      </SectionShell>

      {/* ── Sezione B: consiglieri ─────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Consiglieri"
        subtitle="Roster derivato dai settings (gruppi Base / Infrastruttura / Multi-provider) incrociato con le definizioni sub-agent."
        loading={rosterLoading}
        error={rosterError}
        empty={members.length === 0}
        emptyLabel="Nessuna figura configurata nei settings del consiglio."
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {purposes.error ? (
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Nota: purpose model non caricati ({purposes.error}) — colonna Modello non disponibile.
            </div>
          ) : null}
          {rowError ? <div style={{ fontSize: 12, color: tc.error }}>{rowError}</div> : null}
          <div style={tableWrapStyle(tc)}>
            <table style={{ width: "100%", borderCollapse: "collapse", background: tc.bgCard }}>
              <thead>
                <tr>
                  <th style={thStyle(tc)}>Figura</th>
                  <th style={thStyle(tc)}>Gruppo</th>
                  <th style={thStyle(tc)}>Descrizione</th>
                  <th style={thStyle(tc)}>Modello</th>
                  <th style={thStyle(tc)}>Limiti</th>
                  <th style={thStyle(tc)}>Stato</th>
                  <th style={thStyle(tc)}>Azioni</th>
                </tr>
              </thead>
              <tbody>
                {members.map((m) => {
                  const def = m.definition;
                  const busy = rowBusy === m.kind;
                  return (
                    <tr key={m.kind}>
                      <td style={{ ...tdStyle(tc), fontFamily: "var(--font-mono)", fontWeight: 600, whiteSpace: "nowrap" }}>
                        {m.kind}
                      </td>
                      <td style={tdStyle(tc)}>
                        <Badge tc={tc} tone="info" label={GROUP_LABEL[m.group]} />
                      </td>
                      <td style={{ ...tdStyle(tc), maxWidth: 320 }}>
                        <span title={def?.description ?? undefined} style={{ display: "block", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                          {def?.description ?? "—"}
                        </span>
                      </td>
                      <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>
                        {def ? (
                          <span>
                            <span style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>{def.modelPurpose}</span>
                            {m.purposeModel ? (
                              <span style={{ color: tc.textMuted, fontSize: 11 }}>
                                {" "}
                                · {m.purposeModel.tier ?? "—"} · {m.purposeModel.provider}/{m.purposeModel.model_id}
                              </span>
                            ) : null}
                          </span>
                        ) : (
                          "—"
                        )}
                      </td>
                      <td style={{ ...tdStyle(tc), whiteSpace: "nowrap", fontSize: 11 }}>
                        {def ? `${def.maxIterations} iter · ${def.timeoutS}s` : "—"}
                      </td>
                      <td style={tdStyle(tc)}>
                        <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
                          {def ? (
                            <Badge tc={tc} tone={def.isEnabled ? "ok" : "muted"} label={def.isEnabled ? "Attivo" : "Disabilitato"} />
                          ) : (
                            <Badge tc={tc} tone="warn" label="Definizione mancante" />
                          )}
                          {def && !m.inWhitelist ? <Badge tc={tc} tone="warn" label="Fuori whitelist dispatcher" /> : null}
                          {def && !m.hasAdvisoryVerdict ? <Badge tc={tc} tone="warn" label="Senza advisory_verdict" /> : null}
                        </div>
                      </td>
                      <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>
                        <div style={{ display: "flex", gap: 6 }}>
                          <button
                            type="button"
                            disabled={!def || busy}
                            onClick={() => void openPromptEditor(m)}
                            style={btnStyle(tc, "primary", !def || busy)}
                          >
                            Modifica prompt
                          </button>
                          <button
                            type="button"
                            disabled={!def || busy}
                            onClick={() => def && setDescEditor({ def, text: def.description ?? "", saving: false, error: null })}
                            style={btnStyle(tc, "ghost", !def || busy)}
                          >
                            Descrizione
                          </button>
                          <button
                            type="button"
                            disabled={!def || busy}
                            onClick={() => void toggleEnabled(m)}
                            style={btnStyle(tc, def?.isEnabled ? "danger" : "ghost", !def || busy)}
                          >
                            {busy ? "…" : def?.isEnabled ? "Disabilita" : "Abilita"}
                          </button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      </SectionShell>

      {/* ── Sezione C: composizione ────────────────────────────────────────── */}
      <SectionShell
        tc={tc}
        title="Composizione del consiglio"
        subtitle="Gruppo Base (sempre candidato) e gruppo Infrastruttura (aggiunto sui task infra). La figura multi-provider e' governata dal setting orchestrator.multi_provider_kind."
        loading={rosterLoading}
        error={rosterError}
        empty={candidates.length === 0}
        emptyLabel="Nessuna definizione sub-agent disponibile."
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          <div style={tableWrapStyle(tc)}>
            <table style={{ width: "100%", borderCollapse: "collapse", background: tc.bgCard }}>
              <thead>
                <tr>
                  <th style={thStyle(tc)}>Kind</th>
                  <th style={{ ...thStyle(tc), textAlign: "center" }}>Base</th>
                  <th style={{ ...thStyle(tc), textAlign: "center" }}>Infrastruttura</th>
                  <th style={thStyle(tc)}>Note</th>
                </tr>
              </thead>
              <tbody>
                {candidates.map((d) => {
                  const advisory = d.toolWhitelist.includes("advisory_verdict");
                  return (
                    <tr key={d.kind}>
                      <td style={{ ...tdStyle(tc), fontFamily: "var(--font-mono)", whiteSpace: "nowrap" }}>{d.kind}</td>
                      <td style={{ ...tdStyle(tc), textAlign: "center" }}>
                        <input
                          type="checkbox"
                          checked={effBase.includes(d.kind)}
                          onChange={() => toggleComposition(d.kind, "base")}
                        />
                      </td>
                      <td style={{ ...tdStyle(tc), textAlign: "center" }}>
                        <input
                          type="checkbox"
                          checked={effInfra.includes(d.kind)}
                          onChange={() => toggleComposition(d.kind, "infra")}
                        />
                      </td>
                      <td style={{ ...tdStyle(tc), fontSize: 11 }}>
                        <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
                          {!advisory ? <Badge tc={tc} tone="warn" label="Senza advisory_verdict" /> : null}
                          {!d.isEnabled ? <Badge tc={tc} tone="muted" label="Disabilitato" /> : null}
                          {config.multiProviderKind === d.kind ? <Badge tc={tc} tone="info" label="Multi-provider" /> : null}
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <button
              type="button"
              onClick={() => void saveComposition()}
              disabled={!compDirty || compSaving}
              style={btnStyle(tc, "primary", !compDirty || compSaving)}
            >
              {compSaving ? "Salvataggio…" : "Salva composizione"}
            </button>
            {compDirty ? (
              <button
                type="button"
                onClick={() => {
                  setDraftBase(null);
                  setDraftInfra(null);
                }}
                disabled={compSaving}
                style={btnStyle(tc, "ghost", compSaving)}
              >
                Annulla modifiche
              </button>
            ) : null}
            {compError ? <span style={{ fontSize: 12, color: tc.error }}>{compError}</span> : null}
          </div>
        </div>
      </SectionShell>

      {/* ── Modale editor prompt ───────────────────────────────────────────── */}
      <AdminModal
        open={promptEditor !== null}
        onClose={() => void closePromptEditor()}
        title={promptEditor ? `Prompt di '${promptEditor.kind}'` : undefined}
        maxWidth={880}
      >
        {promptEditor ? (
          promptEditor.loading ? (
            <div style={{ fontSize: 13, color: tc.textMuted }}>Caricamento template…</div>
          ) : promptEditor.loadError ? (
            <div style={{ fontSize: 13, color: tc.error }}>
              {promptEditor.loadError}
              <div style={{ marginTop: 8, color: tc.textMuted }}>
                Se il template non esiste, va creato da{" "}
                <a href="/admin/prompts" style={{ color: tc.accent }}>
                  Template Prompt
                </a>{" "}
                con chiave <code>{promptEditor.promptKey}</code>.
              </div>
            </div>
          ) : promptEditor.template ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", fontSize: 12, color: tc.textMuted }}>
                <code>{promptEditor.promptKey}</code>
                <Badge tc={tc} tone="info" label={`v${promptEditor.template.version}`} />
                <Badge
                  tc={tc}
                  tone={promptEditor.template.is_active ? "ok" : "muted"}
                  label={promptEditor.template.is_active ? "attivo" : "non attivo"}
                />
                <span>agg. {formatDate(promptEditor.template.updated_at)} da {promptEditor.template.updated_by}</span>
              </div>
              <textarea
                value={promptEditor.content}
                onChange={(e) => {
                  const v = e.target.value;
                  setPromptEditor((p) => p && { ...p, content: v });
                }}
                rows={22}
                spellCheck={false}
                style={{
                  width: "100%",
                  fontFamily: "var(--font-mono)",
                  fontSize: 12,
                  lineHeight: 1.5,
                  padding: 12,
                  background: tc.bgCard,
                  color: tc.text,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 8,
                  resize: "vertical",
                  boxSizing: "border-box",
                }}
              />
              <label style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 12 }}>
                <span style={{ color: tc.textMuted }}>Nota di modifica (salvata nello storico)</span>
                <input
                  value={promptEditor.changeNote}
                  onChange={(e) => {
                    const v = e.target.value;
                    setPromptEditor((p) => p && { ...p, changeNote: v });
                  }}
                  placeholder="es. rafforzata la lente sicurezza sul boundary auth"
                  style={fieldStyle(tc)}
                />
              </label>
              <div style={{ fontSize: 12, color: tc.textMuted }}>
                Il salvataggio incrementa la versione e ha effetto immediato sul prossimo sub-run (nessuna cache).
              </div>
              {promptEditor.saveError ? (
                <div style={{ fontSize: 12, color: tc.error }}>{promptEditor.saveError}</div>
              ) : null}
              <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
                <button type="button" onClick={() => void closePromptEditor()} style={btnStyle(tc, "ghost")}>
                  Chiudi
                </button>
                <button
                  type="button"
                  onClick={() => void savePrompt()}
                  disabled={!promptDirty || !promptEditor.content.trim() || promptEditor.saving}
                  style={btnStyle(tc, "primary", !promptDirty || !promptEditor.content.trim() || promptEditor.saving)}
                >
                  {promptEditor.saving ? "Salvataggio…" : "Salva nuova versione"}
                </button>
              </div>

              <div style={{ borderTop: `1px solid ${tc.border}`, paddingTop: 12 }}>
                <h4 style={{ margin: "0 0 8px", fontSize: 13, color: tc.text }}>Storico versioni</h4>
                {promptEditor.history.length === 0 ? (
                  <div style={{ fontSize: 12, color: tc.textMuted }}>
                    Nessuna versione precedente registrata.
                  </div>
                ) : (
                  <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                    {promptEditor.history.map((h) => (
                      <Fragment key={h.id}>
                        <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, flexWrap: "wrap" }}>
                          <Badge tc={tc} tone="muted" label={`v${h.version}`} />
                          <span style={{ color: tc.textMuted }}>
                            {formatDate(h.changed_at)} da {h.changed_by}
                          </span>
                          {h.change_note ? <span style={{ color: tc.text }}>{h.change_note}</span> : null}
                          <button
                            type="button"
                            onClick={() =>
                              setPromptEditor(
                                (p) => p && { ...p, expandedHistoryId: p.expandedHistoryId === h.id ? null : h.id },
                              )
                            }
                            style={btnStyle(tc, "ghost")}
                          >
                            {promptEditor.expandedHistoryId === h.id ? "Nascondi" : "Mostra"}
                          </button>
                          <button
                            type="button"
                            onClick={() => setPromptEditor((p) => p && { ...p, content: h.content })}
                            style={btnStyle(tc, "ghost")}
                            title="Copia questo contenuto nell'editor; il salvataggio creera' una nuova versione"
                          >
                            Carica nell'editor
                          </button>
                        </div>
                        {promptEditor.expandedHistoryId === h.id ? (
                          <pre
                            style={{
                              margin: 0,
                              padding: 10,
                              fontSize: 11,
                              background: tc.bgCard,
                              border: `1px solid ${tc.border}`,
                              borderRadius: 6,
                              maxHeight: 220,
                              overflow: "auto",
                              whiteSpace: "pre-wrap",
                            }}
                          >
                            {h.content}
                          </pre>
                        ) : null}
                      </Fragment>
                    ))}
                  </div>
                )}
              </div>
            </div>
          ) : null
        ) : null}
      </AdminModal>

      {/* ── Modale descrizione figura ──────────────────────────────────────── */}
      <AdminModal
        open={descEditor !== null}
        onClose={() => setDescEditor(null)}
        title={descEditor ? `Descrizione di '${descEditor.def.kind}'` : undefined}
        maxWidth={560}
      >
        {descEditor ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Usata dal dispatcher per la delega per descrizione. Il prompt completo si modifica con "Modifica prompt".
            </div>
            <textarea
              value={descEditor.text}
              onChange={(e) => {
                const v = e.target.value;
                setDescEditor((s) => s && { ...s, text: v });
              }}
              rows={4}
              style={{ ...fieldStyle(tc), fontFamily: "inherit", resize: "vertical" }}
            />
            {descEditor.error ? <div style={{ fontSize: 12, color: tc.error }}>{descEditor.error}</div> : null}
            <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
              <button type="button" onClick={() => setDescEditor(null)} style={btnStyle(tc, "ghost")}>
                Annulla
              </button>
              <button
                type="button"
                onClick={() => void saveDescription()}
                disabled={descEditor.saving}
                style={btnStyle(tc, "primary", descEditor.saving)}
              >
                {descEditor.saving ? "Salvataggio…" : "Salva"}
              </button>
            </div>
          </div>
        ) : null}
      </AdminModal>
    </div>
  );
}

// ── Helper di layout (pattern pagine admin, vedi admin/alignment) ────────────

function SectionShell({
  tc,
  title,
  subtitle,
  loading,
  error,
  empty,
  emptyLabel,
  children,
}: {
  tc: ThemeColors;
  title: string;
  subtitle: string;
  loading: boolean;
  error: string | null;
  empty: boolean;
  emptyLabel: string;
  children: ReactNode;
}) {
  return (
    <section style={{ display: "flex", flexDirection: "column", gap: 12, minWidth: 0 }}>
      <div style={{ minWidth: 0 }}>
        <h3 style={{ fontSize: 15, fontWeight: 700, margin: "0 0 2px", color: tc.text }}>{title}</h3>
        <p style={{ fontSize: 12, color: tc.textMuted, margin: 0 }}>{subtitle}</p>
      </div>
      {loading ? (
        <div style={{ fontSize: 13, color: tc.textMuted, padding: "8px 0" }}>Caricamento…</div>
      ) : error ? (
        <div
          style={{
            fontSize: 13,
            color: tc.error,
            padding: "10px 12px",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
          }}
        >
          Errore: {error}
        </div>
      ) : empty ? (
        <div style={{ fontSize: 13, color: tc.textMuted, padding: "8px 0" }}>{emptyLabel}</div>
      ) : (
        children
      )}
    </section>
  );
}

function tableWrapStyle(tc: ThemeColors): CSSProperties {
  return {
    display: "block",
    overflowX: "auto",
    border: `1px solid ${tc.border}`,
    borderRadius: 8,
    minWidth: 0,
  };
}

function thStyle(tc: ThemeColors): CSSProperties {
  return {
    textAlign: "left",
    padding: "8px 10px",
    fontSize: 11,
    fontWeight: 600,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: tc.textMuted,
    borderBottom: `1px solid ${tc.border}`,
    whiteSpace: "nowrap",
  };
}

function tdStyle(tc: ThemeColors): CSSProperties {
  return {
    padding: "8px 10px",
    fontSize: 12,
    color: tc.text,
    borderBottom: `1px solid ${tc.border}`,
    verticalAlign: "top",
  };
}

function Badge({ tc, label, tone }: { tc: ThemeColors; label: string; tone: "ok" | "warn" | "muted" | "info" }) {
  const palette: Record<string, { bg: string; fg: string }> = {
    ok: { bg: "rgba(74,222,128,0.15)", fg: tc.success },
    warn: { bg: "rgba(248,113,113,0.15)", fg: tc.error },
    info: { bg: tc.accentBg, fg: tc.accent },
    muted: { bg: tc.border, fg: tc.textMuted },
  };
  const { bg, fg } = palette[tone];
  return (
    <span
      style={{
        display: "inline-block",
        flexShrink: 0,
        padding: "2px 8px",
        borderRadius: 6,
        fontSize: 11,
        fontWeight: 600,
        background: bg,
        color: fg,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </span>
  );
}

function ConfigChip({ tc, label, value }: { tc: ThemeColors; label: string; value: string }) {
  return (
    <span
      style={{
        display: "inline-flex",
        gap: 6,
        alignItems: "baseline",
        padding: "4px 10px",
        borderRadius: 8,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        fontSize: 12,
      }}
    >
      <span style={{ color: tc.textMuted }}>{label}</span>
      <span style={{ color: tc.text, fontWeight: 600 }}>{value}</span>
    </span>
  );
}

function fieldStyle(tc: ThemeColors): CSSProperties {
  return {
    padding: "6px 10px",
    background: tc.bgCard,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    fontSize: 12,
    width: "100%",
    boxSizing: "border-box",
  };
}

function btnStyle(tc: ThemeColors, variant: "primary" | "ghost" | "danger", disabled = false): CSSProperties {
  const base: CSSProperties = {
    padding: "5px 12px",
    borderRadius: 6,
    fontSize: 12,
    fontWeight: 600,
    cursor: disabled ? "default" : "pointer",
    opacity: disabled ? 0.5 : 1,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    color: tc.text,
  };
  if (variant === "primary") {
    return { ...base, background: tc.accent, borderColor: tc.accent, color: "#fff" };
  }
  if (variant === "danger") {
    return { ...base, color: tc.error, borderColor: tc.error, background: "transparent" };
  }
  return base;
}
