"use client";
import { useEffect, useState } from "react";
import { TruncatedText } from "../../truncated-text";
import {
  useProjectStore,
  selectServicesMap,
  selectPorts as selectDispatcherPorts,
} from "../../../lib/project-dispatcher/store";
import {
  createRunConfig,
  updateRunConfig,
  deleteRunConfig,
  launchRunConfig,
  detectRunConfigs,
  getProjectServicesStatus,
  controlProjectService,
  getProjectPorts,
} from "../../../lib/api-client";
import type {
  PortEntry,
  ProjectServiceEntry,
  RunConfigItem,
  UserProjectDetails,
} from "../../../lib/api-client";
import {
  type ThemeColors,
  type EditState,
  type SuggestedConfig,
  type Category,
  KIND_OPTIONS,
  KIND_ICON,
  KIND_PLACEHOLDER,
  ROLE_BADGE,
  GROUP_ICON,
  CATEGORY_LABEL,
  CATEGORY_ICON,
  isStopScript,
  isDuplicateOfSystemdService,
  categorize,
} from "./shared";

export function RunDebugView({
  tc,
  project,
  runConfigs,
  onRunConfigsChange,
  onLaunchConfig,
}: {
  tc: ThemeColors;
  project: UserProjectDetails | null;
  runConfigs: RunConfigItem[];
  onRunConfigsChange?: (configs: RunConfigItem[]) => void;
  onLaunchConfig?: (channelId: string) => void;
}) {
  const [editing, setEditing] = useState<EditState | null>(null);
  const [saving, setSaving] = useState(false);
  const [launching, setLaunching] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [suggestions, setSuggestions] = useState<SuggestedConfig[] | null>(null);
  const [selectedSuggestions, setSelectedSuggestions] = useState<Set<number>>(new Set());
  const [importingAll, setImportingAll] = useState(false);
  /** Set degli "short" name dei servizi systemd installati per il progetto.
   *  Usato per nascondere i run config che li duplicherebbero. */
  const [systemdShorts, setSystemdShorts] = useState<Set<string>>(new Set());
  /** Stato live dei servizi systemd del progetto (per sezione "Servizi"). */
  const [systemdSvcs, setSystemdSvcs] = useState<ProjectServiceEntry[]>([]);
  /** Porte rilevate (per link "Apri"). */
  const [ports, setPorts] = useState<PortEntry[]>([]);
  const [svcBusy, setSvcBusy] = useState<Record<string, boolean>>({});
  const [svcMsg, setSvcMsg] = useState<string>("");
  /** Categorie collassate dall'utente (chiavi in CATEGORY_LABEL) */
  const [collapsed, setCollapsed] = useState<Set<Category>>(() => new Set());
  /** "show all" forza il rendering di tutto (anche stop e duplicati) per debug */
  const [showAll, setShowAll] = useState(false);

  const projectId = project?.id ?? "";

  // ── Event-driven: refresh servizi via dispatcher SSE ──
  const servicesFromDispatcher = useProjectStore(selectServicesMap);
  const portsFromDispatcher = useProjectStore(selectDispatcherPorts);

  // Polling rilassato (30s) dei servizi systemd — fallback di sicurezza.
  // I refresh reali sono triggerati da ServiceStarted/Stopped/Restarted sotto.
  useEffect(() => {
    if (!projectId) { setSystemdShorts(new Set()); return; }
    let cancelled = false;
    const refresh = async () => {
      try {
        const r = await getProjectServicesStatus(projectId);
        if (cancelled) return;
        const svcs = r.services ?? [];
        setSystemdShorts(new Set(svcs.map(s => s.short)));
        setSystemdSvcs(svcs);
      } catch { /* ignora */ }
    };
    refresh();
    const t = window.setInterval(refresh, 30_000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [projectId]);

  // Refresh immediato servizi quando il dispatcher notifica un cambio di stato
  useEffect(() => {
    if (!projectId || Object.keys(servicesFromDispatcher).length === 0) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await getProjectServicesStatus(projectId);
        if (cancelled) return;
        const svcs = r.services ?? [];
        setSystemdShorts(new Set(svcs.map(s => s.short)));
        setSystemdSvcs(svcs);
      } catch { /* ignora */ }
    })();
    return () => { cancelled = true; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [servicesFromDispatcher]);

  // Polling rilassato (30s) porte — fallback di sicurezza.
  useEffect(() => {
    if (!projectId) { setPorts([]); return; }
    let cancelled = false;
    const refresh = async () => {
      try {
        const r = await getProjectPorts(projectId);
        if (!cancelled) setPorts(r.ports ?? []);
      } catch { /* ignora */ }
    };
    refresh();
    const t = window.setInterval(refresh, 30_000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [projectId]);

  // Refresh immediato porte quando il dispatcher notifica PortAllocated/Released
  useEffect(() => {
    if (!projectId || portsFromDispatcher.length === 0) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await getProjectPorts(projectId);
        if (!cancelled) setPorts(r.ports ?? []);
      } catch { /* ignora */ }
    })();
    return () => { cancelled = true; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [portsFromDispatcher]);

  const svcUrlFor = (svc: ProjectServiceEntry): string | null => {
    const p =
      ports.find(pp => pp.service === svc.short)
      ?? ports.find(pp =>
        (pp.label ?? "").toLowerCase().includes(svc.short.toLowerCase())
        || (pp.label ?? "").toLowerCase().includes(svc.unit.replace(".service", "").toLowerCase())
      );
    return p?.url ?? null;
  };

  const handleSvcAction = async (svc: ProjectServiceEntry, action: "start" | "stop" | "restart") => {
    if (!projectId) return;
    const key = `${svc.unit}-${action}`;
    setSvcBusy(p => ({ ...p, [key]: true }));
    setSvcMsg("");
    try {
      const r = await controlProjectService(projectId, svc.short, action);
      setSvcMsg(r.ok ? `${svc.short}: ${action} completato` : `${svc.short}: errore (${r.stderr || "fallito"})`);
      // Refresh immediato per aggiornare stato/porte
      setTimeout(async () => {
        try {
          const s = await getProjectServicesStatus(projectId);
          setSystemdSvcs(s.services ?? []);
          setSystemdShorts(new Set((s.services ?? []).map(x => x.short)));
        } catch { /* ignore */ }
        try {
          const p = await getProjectPorts(projectId);
          setPorts(p.ports ?? []);
        } catch { /* ignore */ }
      }, 900);
    } catch {
      setSvcMsg(`${svc.short}: errore di rete`);
    } finally {
      setSvcBusy(p => ({ ...p, [key]: false }));
      setTimeout(() => setSvcMsg(""), 6000);
    }
  };

  const toggleCategory = (cat: Category) => {
    setCollapsed(prev => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat); else next.add(cat);
      return next;
    });
  };

  const startNew = () => setEditing({
    label: "",
    kind: "shell",
    command: "",
    args: "",
    cwd: "",
  });

  const startEdit = (c: RunConfigItem) => setEditing({
    id: c.id,
    label: c.label,
    kind: c.kind,
    command: c.command ?? "",
    args: (c.args ?? []).join(" "),
    cwd: c.cwd ?? "",
  });

  const handleSave = async () => {
    if (!editing || !projectId) return;
    setSaving(true);
    setError(null);
    try {
      const body = {
        label: editing.label,
        kind: editing.kind,
        command: editing.command,
        args: editing.args.split(/\s+/).filter(Boolean),
        cwd: editing.cwd || undefined,
      };
      if (editing.id) {
        await updateRunConfig(projectId, editing.id, body);
        onRunConfigsChange?.(runConfigs.map(c => c.id === editing.id ? { ...c, ...body } : c));
      } else {
        const created = await createRunConfig(projectId, body);
        onRunConfigsChange?.([...runConfigs, created]);
      }
      setEditing(null);
    } catch {
      setError("Errore nel salvataggio");
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id: string) => {
    if (!projectId) return;
    try {
      await deleteRunConfig(projectId, id);
      onRunConfigsChange?.(runConfigs.filter(c => c.id !== id));
    } catch {
      setError("Errore nella cancellazione");
    }
  };

  const handleLaunch = async (id: string) => {
    if (!projectId) return;
    setLaunching(id);
    setError(null);
    try {
      const res = await launchRunConfig(projectId, id);
      onLaunchConfig?.(res.channelId);
    } catch {
      setError("Errore nell'avvio");
    } finally {
      setLaunching(null);
    }
  };

  const handleDetect = async (useAi: boolean = false, force: boolean = false) => {
    if (!projectId) return;
    setDetecting(true);
    setError(null);
    try {
      const res = await detectRunConfigs(projectId, { useAi, force });
      setSuggestions(res.suggestions);
      // Pre-seleziona solo gli essenziali; se nessuno è marcato tale, fallback su tutti.
      const essentialIdx = res.suggestions
        .map((s, i) => (s.essential ? i : -1))
        .filter(i => i >= 0);
      const preselect = essentialIdx.length > 0
        ? essentialIdx
        : res.suggestions.map((_, i) => i);
      setSelectedSuggestions(new Set(preselect));
    } catch {
      setError("Errore nel rilevamento automatico");
    } finally {
      setDetecting(false);
    }
  };

  const handleImportSelected = async (opts: { launchEssentials?: boolean } = {}) => {
    if (!suggestions || !projectId) return;
    setImportingAll(true);
    setError(null);
    const toImport = suggestions.filter((_, i) => selectedSuggestions.has(i));
    const created: RunConfigItem[] = [];
    try {
      for (const s of toImport) {
        const body = {
          label: s.label,
          kind: s.kind,
          command: s.command,
          args: s.args ?? [],
          cwd: s.cwd,
          env: s.env ?? {},
          role: s.role ?? undefined,
          essential: s.essential ?? false,
          group: s.group ?? undefined,
        };
        const c = await createRunConfig(projectId, body);
        created.push(c);
      }
      onRunConfigsChange?.([...runConfigs, ...created]);
      setSuggestions(null);
      if (opts.launchEssentials) {
        for (const c of created) {
          if (!c.essential) continue;
          try {
            const res = await launchRunConfig(projectId, c.id);
            onLaunchConfig?.(res.channelId);
          } catch {
            // continua con gli altri anche se uno fallisce
          }
        }
      }
    } catch {
      setError("Errore durante l'importazione");
    } finally {
      setImportingAll(false);
    }
  };

  const _handleLaunchAllEssentials = async () => {
    if (!projectId) return;
    setError(null);
    for (const c of runConfigs) {
      if (!c.essential) continue;
      try {
        const res = await launchRunConfig(projectId, c.id);
        onLaunchConfig?.(res.channelId);
      } catch {
        // continua
      }
    }
  };

  // Raggruppa i suggerimenti per `group` mantenendo l'indice originale (serve per la selezione).
  const groupedSuggestions: Array<{ group: string; items: Array<{ s: SuggestedConfig; idx: number }> }> = (() => {
    if (!suggestions) return [];
    const map = new Map<string, Array<{ s: SuggestedConfig; idx: number }>>();
    suggestions.forEach((s, idx) => {
      const key = (s.group as string | undefined) ?? "altro";
      if (!map.has(key)) map.set(key, []);
      map.get(key)!.push({ s, idx });
    });
    return Array.from(map.entries()).map(([group, items]) => ({ group, items }));
  })();

  const _essentialCount = runConfigs.filter(c => c.essential).length;

  const inputStyle = {
    width: "100%",
    padding: "5px 8px",
    borderRadius: 6,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontSize: 12,
    fontFamily: "inherit",
    boxSizing: "border-box" as const,
  };

  const labelStyle = { fontSize: 11, color: tc.textMuted, marginBottom: 3, display: "block" as const };

  return (
    <>
      <div className="flex-row px-3 py-2" style={{ borderBottom: `1px solid ${tc.border}`, justifyContent: "space-between" }} title="Script & comandi una-tantum del progetto (build, test, lint, migrazioni, ecc.). Gli script che corrispondono a servizi systemd installati e quelli di stop/kill vengono nascosti automaticamente — clicca '👁' per mostrarli.">
        <span style={{ fontSize: 11, fontWeight: 600, color: tc.textMuted, textTransform: "uppercase", letterSpacing: 1 }}>Script &amp; Comandi</span>
        {!editing && !suggestions && (
          <div className="flex-row" style={{ gap: 4, alignItems: "center" }}>
            <button
              onClick={() => setShowAll(v => !v)}
              title={showAll
                ? "Nascondi script di stop/kill e duplicati di servizi systemd"
                : "Mostra TUTTI gli script (anche stop e quelli coperti da servizi systemd)"}
              className="text-xs px-1 py-0 rounded-sm cursor-pointer"
              style={{
                background: showAll ? tc.accent : "none",
                color: showAll ? "#fff" : tc.textMuted,
                border: `1px solid ${tc.border}`,
                fontSize: 10, padding: "1px 6px",
              }}
            >{showAll ? "🙈 filtra" : "👁 tutti"}</button>
            <button
              onClick={() => handleDetect(false, false)}
              disabled={!project || detecting}
              title="Rileva configurazioni di avvio dai file del progetto (usa cache se disponibile)"
              className="text-xs px-1 py-0 rounded-sm cursor-pointer" style={{ background: "none", border: `1px solid ${tc.border}`, color: tc.accent }}
            >{detecting ? "…" : "✨ Auto"}</button>
            <button
              onClick={() => handleDetect(false, true)}
              disabled={!project || detecting}
              title="Forza riscansione del filesystem (ignora cache)"
              className="text-xs px-1 py-0 rounded-sm cursor-pointer" style={{ background: "none", border: `1px solid ${tc.border}`, color: tc.accent }}
            >↺</button>
            <button
              onClick={startNew}
              disabled={!project}
              title="Nuova configurazione manuale"
              style={{ background: "none", border: "none", color: tc.accent, cursor: project ? "pointer" : "not-allowed", fontSize: 18, lineHeight: 1, padding: "0 2px" }}
            >+</button>
          </div>
        )}
      </div>

      <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: 10, display: "flex", flexDirection: "column", gap: 8 }}>
        {error && <div style={{ color: tc.error, fontSize: 11, padding: "4px 0" }}>{error}</div>}

        {/* Servizi (systemd) — shortcut operativi. NON lanciano npm direttamente. */}
        {projectId && (
          <div style={{ border: `1px solid ${tc.border}`, borderRadius: 10, background: tc.bgCard, padding: 10 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 6 }}>
              <div style={{ fontSize: 11, fontWeight: 800, color: tc.textMuted, textTransform: "uppercase", letterSpacing: "0.06em" }}>
                Servizi
              </div>
              <div style={{ fontSize: 10, color: tc.textMuted }}>
                {systemdSvcs.length > 0 ? `${systemdSvcs.length}` : "0"}
              </div>
            </div>
            {systemdSvcs.length === 0 ? (
              <div style={{ color: tc.textMuted, fontSize: 12 }}>
                Nessun servizio systemd installato. Usa <strong>Run &amp; Debug → + Configura</strong> per crearli.
              </div>
            ) : (
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                {systemdSvcs.map((svc) => {
                  const url = svcUrlFor(svc);
                  const isActive = svc.state === "active";
                  const startBusy = !!svcBusy[`${svc.unit}-start`];
                  const stopBusy = !!svcBusy[`${svc.unit}-stop`];
                  const rstBusy = !!svcBusy[`${svc.unit}-restart`];
                  const anyBusy = startBusy || stopBusy || rstBusy;
                  return (
                    <div key={svc.unit} style={{ border: `1px solid ${tc.border}`, borderRadius: 8, padding: "8px 10px" }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span
                          title={`${svc.state}${svc.sub ? ` (${svc.sub})` : ""}`}
                          style={{
                            width: 7, height: 7, borderRadius: "50%",
                            background: isActive ? "#22c55e" : (svc.state === "failed" ? "#ef4444" : "#6b7280"),
                            display: "inline-block", flexShrink: 0,
                          }}
                        />
                        <div style={{ flex: 1, minWidth: 0 }}>
                          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                            <span style={{ fontSize: 12, fontWeight: 700, color: tc.text, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                              {svc.short}
                            </span>
                            <span style={{ fontSize: 10, color: tc.textMuted, fontFamily: '"JetBrains Mono", monospace' }}>
                              {svc.state}{svc.sub ? `/${svc.sub}` : ""}
                            </span>
                          </div>
                          {url && (
                            <a
                              href={url}
                              target="_blank"
                              rel="noreferrer"
                              style={{ fontSize: 10, color: tc.accent, textDecoration: "none", fontFamily: '"JetBrains Mono", monospace' }}
                            >
                              {url}
                            </a>
                          )}
                        </div>
                        <div style={{ display: "flex", gap: 4, flexShrink: 0 }}>
                          {!isActive && (
                            <button
                              onClick={() => handleSvcAction(svc, "start")}
                              disabled={anyBusy}
                              style={{ background: "transparent", border: `1px solid #22c55e`, color: "#22c55e", borderRadius: 6, padding: "2px 8px", fontSize: 11, cursor: anyBusy ? "wait" : "pointer", opacity: anyBusy ? 0.6 : 1 }}
                            >
                              {startBusy ? "…" : "start"}
                            </button>
                          )}
                          {isActive && (
                            <button
                              onClick={() => handleSvcAction(svc, "stop")}
                              disabled={anyBusy}
                              style={{ background: "transparent", border: `1px solid #ef4444`, color: "#ef4444", borderRadius: 6, padding: "2px 8px", fontSize: 11, cursor: anyBusy ? "wait" : "pointer", opacity: anyBusy ? 0.6 : 1 }}
                            >
                              {stopBusy ? "…" : "stop"}
                            </button>
                          )}
                          <button
                            onClick={() => handleSvcAction(svc, "restart")}
                            disabled={anyBusy}
                            style={{ background: "transparent", border: `1px solid #f59e0b`, color: "#f59e0b", borderRadius: 6, padding: "2px 8px", fontSize: 11, cursor: anyBusy ? "wait" : "pointer", opacity: anyBusy ? 0.6 : 1 }}
                          >
                            {rstBusy ? "…" : "restart"}
                          </button>
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
            {svcMsg && (
              <div style={{ fontSize: 11, marginTop: 8, color: svcMsg.includes("errore") ? tc.error : "#22c55e" }}>
                {svcMsg}
              </div>
            )}
          </div>
        )}

        {/* Auto-detect suggestions panel */}
        {suggestions && (
          <div style={{ border: `1px solid ${tc.accent}`, borderRadius: 8, padding: 10, background: tc.bgCard, display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span className="text-sm font-semibold text-accent">✨ Configurazioni rilevate</span>
              <button onClick={() => setSuggestions(null)} style={{ background: "none", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 14 }}>×</button>
            </div>
            {suggestions.length === 0 ? (
              <div className="text-xs text-muted">Nessuna configurazione rilevata in questo progetto.</div>
            ) : (
              <>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 2 }}>
                  Selezione iniziale: solo gli <strong style={{ color: tc.accent }}>essenziali</strong> per avviare l&apos;app. Spunta altre voci per aggiungerle.
                </div>
                {groupedSuggestions.map(({ group, items }) => {
                  const groupIndices = items.map(it => it.idx);
                  const allSelected = groupIndices.every(i => selectedSuggestions.has(i));
                  const toggleGroup = (mode: "all" | "essential" | "none") => {
                    const next = new Set(selectedSuggestions);
                    groupIndices.forEach(i => next.delete(i));
                    if (mode === "all") groupIndices.forEach(i => next.add(i));
                    else if (mode === "essential") {
                      items.forEach(it => { if (it.s.essential) next.add(it.idx); });
                    }
                    setSelectedSuggestions(next);
                  };
                  return (
                    <div key={group} className="mt-6 pt-2" style={{ borderTop: `1px solid ${tc.border}` }}>
                      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 4 }}>
                        <span className="text-xs font-bold" style={{ color: tc.textSecondary }}>
                          {GROUP_ICON(group)} {group}
                        </span>
                        <span style={{ fontSize: 10, color: tc.textMuted, display: "flex", gap: 4 }}>
                          <button onClick={() => toggleGroup("all")} style={{ background: "none", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 10, padding: "0 2px" }} title="Seleziona tutti">{allSelected ? "✓ tutti" : "tutti"}</button>
                          <button onClick={() => toggleGroup("essential")} style={{ background: "none", border: "none", color: tc.accent, cursor: "pointer", fontSize: 10, padding: "0 2px" }} title="Solo essenziali">essenziali</button>
                          <button onClick={() => toggleGroup("none")} style={{ background: "none", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 10, padding: "0 2px" }} title="Nessuno">nessuno</button>
                        </span>
                      </div>
                      {items.map(({ s, idx }) => {
                        const role = (s.role as string) ?? "tool";
                        const badge = ROLE_BADGE[role] ?? ROLE_BADGE.tool;
                        return (
                          <label key={idx} className="flex-row-gap-8 cursor-pointer" style={{ alignItems: "flex-start", padding: "3px 0" }}>
                            <input type="checkbox" checked={selectedSuggestions.has(idx)}
                              onChange={e => {
                                const next = new Set(selectedSuggestions);
                                if (e.target.checked) next.add(idx); else next.delete(idx);
                                setSelectedSuggestions(next);
                              }}
                              style={{ marginTop: 2, flexShrink: 0 }}
                            />
                            <div className="flex-1" style={{ minWidth: 0 }}>
                              <div style={{ fontSize: 12, fontWeight: 600, color: tc.text, display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                                <span>{KIND_ICON[s.kind] ?? "⚡"} {s.label}</span>
                                <span title={`Ruolo: ${badge.label}`} style={{ fontSize: 9, padding: "1px 5px", borderRadius: 4, background: badge.color + "33", color: badge.color, fontWeight: 700 }}>
                                  {badge.icon} {badge.label}
                                </span>
                                {s.essential && (
                                  <span title="Essenziale per l'avvio" style={{ fontSize: 9, padding: "1px 5px", borderRadius: 4, background: "#22c55e33", color: "#22c55e", fontWeight: 700 }}>essenziale</span>
                                )}
                              </div>
                              <TruncatedText
                                text={s.command + (s.args?.length ? " " + s.args.join(" ") : "")}
                                tc={tc}
                                style={{
                                  fontFamily: '"JetBrains Mono", monospace',
                                  fontSize: 10,
                                  color: tc.textMuted,
                                }}
                              />
                            </div>
                          </label>
                        );
                      })}
                    </div>
                  );
                })}
                <div style={{ display: "flex", gap: 6, marginTop: 8, flexWrap: "wrap" }}>
                  <button
                    onClick={() => handleImportSelected({ launchEssentials: false })}
                    disabled={importingAll || selectedSuggestions.size === 0}
                    style={{ flex: "1 1 45%", background: tc.accent, color: "#fff", border: "none", borderRadius: 6, padding: "5px 0", fontSize: 12, cursor: selectedSuggestions.size === 0 ? "not-allowed" : "pointer", opacity: selectedSuggestions.size === 0 ? 0.5 : 1 }}
                  >
                    {importingAll ? "Importo..." : `Importa (${selectedSuggestions.size})`}
                  </button>
                  <button onClick={() => handleDetect(true)} disabled={detecting}
                    title="Rifinisci la classificazione con Nexus AI"
                    style={{ flex: "1 1 45%", background: "none", color: tc.accent, border: `1px solid ${tc.accent}`, borderRadius: 6, padding: "5px 0", fontSize: 11, cursor: "pointer" }}>
                    🤖 Rifinisci con AI
                  </button>
                  <button onClick={() => setSuggestions(null)}
                    style={{ flex: "1 1 45%", background: "none", color: tc.textSecondary, border: `1px solid ${tc.border}`, borderRadius: 6, padding: "5px 0", fontSize: 12, cursor: "pointer" }}>
                    Annulla
                  </button>
                </div>
              </>
            )}
          </div>
        )}

        {/* Edit / Create form */}
        {editing && (
          <div style={{ border: `1px solid ${tc.accent}`, borderRadius: 8, padding: 10, background: tc.bgCard, display: "flex", flexDirection: "column", gap: 8 }}>
            <div style={{ fontSize: 12, fontWeight: 600, color: tc.accent, marginBottom: 2 }}>
              {editing.id ? "Modifica configurazione" : "Nuova configurazione"}
            </div>
            <div>
              <label style={labelStyle}>Nome</label>
              <input style={inputStyle} placeholder="es. Dev Server" value={editing.label}
                onChange={e => setEditing(prev => prev ? { ...prev, label: e.target.value } : null)} />
            </div>
            <div>
              <label style={labelStyle}>Tipo</label>
              <select style={inputStyle} value={editing.kind}
                onChange={e => setEditing(prev => prev ? { ...prev, kind: e.target.value, command: "" } : null)}>
                {KIND_OPTIONS.map(o => <option key={o.value} value={o.value}>{o.label}</option>)}
              </select>
            </div>
            <div>
              <label style={labelStyle}>Comando</label>
              <input style={{ ...inputStyle, fontFamily: '"JetBrains Mono", monospace' }}
                placeholder={KIND_PLACEHOLDER[editing.kind] ?? "comando"}
                value={editing.command}
                onChange={e => setEditing(prev => prev ? { ...prev, command: e.target.value } : null)} />
            </div>
            <div>
              <label style={labelStyle}>Argomenti (separati da spazio)</label>
              <input style={{ ...inputStyle, fontFamily: '"JetBrains Mono", monospace' }}
                placeholder="--port 3000 --watch"
                value={editing.args}
                onChange={e => setEditing(prev => prev ? { ...prev, args: e.target.value } : null)} />
            </div>
            <div>
              <label style={labelStyle}>Working directory (opzionale)</label>
              <input style={{ ...inputStyle, fontFamily: '"JetBrains Mono", monospace' }}
                placeholder="lascia vuoto = root progetto"
                value={editing.cwd}
                onChange={e => setEditing(prev => prev ? { ...prev, cwd: e.target.value } : null)} />
            </div>
            <div style={{ display: "flex", gap: 6, marginTop: 2 }}>
              <button onClick={handleSave} disabled={saving || !editing.label || !editing.command}
                style={{ flex: 1, background: tc.accent, color: "#fff", border: "none", borderRadius: 6, padding: "5px 0", fontSize: 12, cursor: saving ? "wait" : "pointer" }}>
                {saving ? "Salvo..." : "Salva"}
              </button>
              <button onClick={() => { setEditing(null); setError(null); }}
                style={{ flex: 1, background: "none", color: tc.textSecondary, border: `1px solid ${tc.border}`, borderRadius: 6, padding: "5px 0", fontSize: 12, cursor: "pointer" }}>
                Annulla
              </button>
            </div>
          </div>
        )}

        {/* Config list categorizzata + filtrata */}
        {runConfigs.length === 0 && !editing ? (
          <div style={{ color: tc.textMuted, fontSize: 12, paddingTop: 4 }}>
            Nessuna configurazione. Clicca <strong>+</strong> per aggiungerne una.
          </div>
        ) : (() => {
          const renderCard = (config: RunConfigItem, mutedReason?: string) => {
            const cmdFull = `${config.command}${config.args?.length ? " " + config.args.join(" ") : ""}`;
            const rootPath = project?.rootPath ?? "";
            const rel = config.cwd && rootPath && config.cwd.startsWith(rootPath)
              ? config.cwd.slice(rootPath.length).replace(/^\//, "") || "."
              : (config.cwd ? config.cwd.split("/").slice(-2).join("/") : null);
            const cardStyle: React.CSSProperties = {
              border: `1px solid ${tc.border}`, borderRadius: 8, background: tc.bgCard,
              padding: "8px 10px", marginBottom: 6, opacity: mutedReason ? 0.55 : 1,
            };
            return (
              <div key={config.id} style={cardStyle}>
                <div className="flex-row-gap-6">
                  <span style={{ fontSize: 14 }}>{KIND_ICON[config.kind] ?? "⚡"}</span>
                  <span title={mutedReason ? `${config.label}\n\n${mutedReason}` : config.label}
                    style={{ fontSize: 13, fontWeight: 600, color: tc.text, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {config.label}
                  </span>
                  <button onClick={() => handleLaunch(config.id)} disabled={!!launching} title={mutedReason ?? "Avvia"}
                    style={{ background: "#22c55e", border: "none", borderRadius: 5, color: "#fff", width: 24, height: 24, cursor: launching ? "wait" : "pointer", fontSize: 12, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
                    {launching === config.id ? "…" : "▶"}
                  </button>
                  <button onClick={() => startEdit(config)} title="Modifica"
                    style={{ background: "none", border: `1px solid ${tc.border}`, borderRadius: 5, color: tc.textMuted, width: 24, height: 24, cursor: "pointer", fontSize: 12, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
                    ✎
                  </button>
                  <button onClick={() => handleDelete(config.id)} title="Elimina"
                    style={{ background: "none", border: `1px solid ${tc.border}`, borderRadius: 5, color: tc.error, width: 24, height: 24, cursor: "pointer", fontSize: 13, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
                    ×
                  </button>
                </div>
                <div title={cmdFull} style={{ fontFamily: '"JetBrains Mono", monospace', fontSize: 11, color: tc.textMuted, marginTop: 4, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {cmdFull}
                </div>
                {rel && (
                  <div title={config.cwd} style={{ fontSize: 10, color: tc.textMuted, marginTop: 2, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    📁 {rel}
                  </div>
                )}
                {mutedReason && (
                  <div style={{ fontSize: 10, color: tc.accent, marginTop: 4, fontStyle: "italic" }}>
                    {mutedReason}
                  </div>
                )}
              </div>
            );
          };

          // Pre-filtra per stop/duplicati. In modalità showAll, mostra tutto come "muted" (visibile ma sbiadito).
          const visible: Array<{ c: RunConfigItem; muted?: string }> = [];
          for (const c of runConfigs) {
            const isStop = isStopScript(c);
            const isDup  = isDuplicateOfSystemdService(c, systemdShorts);
            if ((isStop || isDup) && !showAll) continue;
            const muted = showAll
              ? (isStop ? "Script di stop/kill (lancialo solo se sai cosa fai)" :
                 isDup  ? "Duplicato di un servizio systemd installato" : undefined)
              : undefined;
            visible.push({ c, muted });
          }

          // Raggruppa per categoria
          const grouped: Record<Category, Array<{ c: RunConfigItem; muted?: string }>> = {
            dev: [], build: [], test: [], lint: [], db: [], altro: [],
          };
          for (const v of visible) grouped[categorize(v.c)].push(v);

          const totalHidden = runConfigs.length - visible.length;
          const order: Category[] = ["dev", "build", "test", "lint", "db", "altro"];

          if (visible.length === 0) {
            return (
              <div style={{ color: tc.textMuted, fontSize: 12, paddingTop: 4 }}>
                Tutti gli {runConfigs.length} script rilevati sono nascosti perché duplicati di servizi systemd o script di stop. Clicca <strong>👁 tutti</strong> in alto per vederli.
              </div>
            );
          }

          return (
            <>
              {totalHidden > 0 && !showAll && (
                <div style={{ fontSize: 10, color: tc.textMuted, marginBottom: 6, fontStyle: "italic" }}>
                  ℹ {totalHidden} script nascosti (duplicati systemd o stop). Clicca 👁 in alto per mostrarli.
                </div>
              )}
              {order.map(cat => {
                const items = grouped[cat];
                if (items.length === 0) return null;
                const isCollapsed = collapsed.has(cat);
                return (
                  <div key={cat} style={{ marginBottom: 8 }}>
                    <button
                      onClick={() => toggleCategory(cat)}
                      style={{
                        background: "none", border: "none", width: "100%",
                        display: "flex", alignItems: "center", gap: 6,
                        padding: "4px 2px", color: tc.textMuted,
                        fontSize: 10, fontWeight: 700, textTransform: "uppercase",
                        letterSpacing: "0.06em", cursor: "pointer", textAlign: "left",
                      }}
                      title={isCollapsed ? "Espandi" : "Comprimi"}
                    >
                      <span style={{ fontSize: 11 }}>{isCollapsed ? "▶" : "▼"}</span>
                      <span>{CATEGORY_ICON[cat]} {CATEGORY_LABEL[cat]}</span>
                      <span style={{ marginLeft: "auto", fontSize: 10, color: tc.textMuted }}>{items.length}</span>
                    </button>
                    {!isCollapsed && items.map(({ c, muted }) => renderCard(c, muted))}
                  </div>
                );
              })}
            </>
          );
        })()}
      </div>
    </>
  );
}
