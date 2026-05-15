"use client";
import { useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { shortenAbsolutePath } from "../../lib/format";
import { TruncatedText } from "../truncated-text";
import { DocumentsSidebar } from "./documents-sidebar";
import { ProjectExplorer } from "../project-explorer";
import { SourceControlPanel } from "../git/source-control-panel";
import { ServerMonitorPanel } from "./server-monitor-panel";
import { ProjectDbPanel } from "../project-db/project-db-panel";
import {
  createRunConfig,
  updateRunConfig,
  deleteRunConfig,
  launchRunConfig,
  detectRunConfigs,
  getProjectServicesStatus,
  controlProjectService,
  getProjectPorts,
} from "../../lib/api-client";
import type {
  EditorTabState,
  GitRepositoryState,
  PortEntry,
  ProjectServiceEntry,
  RunConfigItem,
  UserProjectDetails,
  WorkspaceTreeNode,
} from "../../lib/api-client";

export type SidebarView =
  | "explorer"
  | "search"
  | "source-control"
  | "run"
  | "docs"
  | "server-monitor"
  | "project-db";

export interface SidebarManagerProps {
  activeSidebarView: SidebarView;
  project: UserProjectDetails | null;
  treeNodes: WorkspaceTreeNode[];
  git: GitRepositoryState | null;
  activeEditorTab: EditorTabState | null;
  allOpenTabs: EditorTabState[];
  currentBranch: string;
  runConfigs: RunConfigItem[];
  onRunConfigsChange?: (configs: RunConfigItem[]) => void;
  onLaunchConfig?: (channelId: string) => void;
  searchQuery: string;
  searchBusy: boolean;
  searchResults: Array<{ path: string; line: number; column: number; preview: string }>;
  onSetSearchQuery: (q: string) => void;
  onSearch: () => void;
  onOpenFile: (path: string, line?: number, groupId?: string) => void;
  onSaveActive: () => void;
  onCreateEntry: (kind: "file" | "directory") => void;
  onRefreshProject: () => void | Promise<void>;
  onProjectAnalyzed?: () => void;
  onSendToChat?: (msg: string, options?: { providerHint?: string; modelHint?: string }) => void;
}

function iconButton(
  tc: ReturnType<typeof useThemeColors>,
  disabled = false,
) {
  return {
    width: 30,
    height: 30,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgInput : tc.bgCard,
    color: disabled ? tc.textMuted : tc.textSecondary,
    borderRadius: 7,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    lineHeight: 1,
  } as const;
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    padding: "7px 10px",
    fontSize: 12,
    boxSizing: "border-box",
  } as const;
}

function listRowButton(tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    textAlign: "left",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    cursor: "pointer",
    display: "flex",
    flexDirection: "column",
    gap: 4,
  } as const;
}

const KIND_OPTIONS = [
  { value: "shell", label: "Shell" },
  { value: "npm", label: "npm" },
  { value: "cargo", label: "Cargo" },
  { value: "python", label: "Python" },
  { value: "node", label: "Node" },
];

const KIND_ICON: Record<string, string> = {
  shell: "⚡",
  npm: "📦",
  cargo: "🦀",
  python: "🐍",
  node: "🟩",
  playwright: "🐛",
};

const KIND_PLACEHOLDER: Record<string, string> = {
  shell: "bash script.sh",
  npm: "npm run dev",
  cargo: "cargo run",
  python: "python main.py",
  node: "node index.js",
};

const ROLE_BADGE: Record<string, { icon: string; label: string; color: string }> = {
  frontend: { icon: "⚛️", label: "frontend", color: "#60a5fa" },
  backend:  { icon: "🔧", label: "backend",  color: "#a78bfa" },
  service:  { icon: "🐳", label: "servizio", color: "#22d3ee" },
  test:     { icon: "🧪", label: "test",     color: "#f59e0b" },
  tool:     { icon: "🛠️", label: "tool",     color: "#9ca3af" },
};

const GROUP_ICON = (group: string | undefined | null): string => {
  if (!group) return "📁";
  if (group === "docker") return "🐳";
  if (group.startsWith("crates") || group.startsWith("cargo")) return "🦀";
  if (group.startsWith("playwright")) return "🐛";
  if (group === "python") return "🐍";
  if (group === "make") return "🛠️";
  if (group === "go" || group === "dotnet") return "⚙️";
  return "📦";
};

type EditState = {
  id?: string;
  label: string;
  kind: string;
  command: string;
  args: string;
  cwd: string;
};

type SuggestedConfig = Omit<RunConfigItem, "id"> & { args?: string[] };

// ── Filtraggio + Categorizzazione Run Configs ────────────────────────────
// L'auto-detect propone tutto quel che trova nel filesystem (npm scripts, make
// targets, ecc.), ma molte voci si sovrappongono ai servizi systemd persistenti
// del progetto (gestiti nel pannello inferiore Run & Debug) e altre sono
// "stop / kill" che non hanno senso come bottone "▶ Avvia".

const STOP_PATTERNS = [
  /-stop\b/i, /:stop\b/i, /^stop[-_:]/i,
  /\bkill\b/i, /^docker\s+stop\b/i, /^docker[-_]?compose\s+down\b/i,
  /\bteardown\b/i, /-down\b/i, /:down\b/i,
];

/** True se il run config è uno script di stop/kill (non andrebbe lanciato col tasto ▶). */
function isStopScript(c: { label: string; command?: string; args?: string[] }): boolean {
  const haystack = `${c.label} ${c.command ?? ""} ${(c.args ?? []).join(" ")}`;
  return STOP_PATTERNS.some(re => re.test(haystack));
}

/** True se il run config duplica un servizio systemd già installato (per nome breve). */
function isDuplicateOfSystemdService(
  c: { label: string; command?: string; args?: string[] },
  serviceShorts: Set<string>,
): boolean {
  if (serviceShorts.size === 0) return false;
  const text = `${c.label} ${(c.args ?? []).join(" ")}`.toLowerCase();
  for (const short of serviceShorts) {
    const s = short.toLowerCase();
    if (s.length < 3) continue;
    // Match testuale solo se la parola completa è presente (no "be" che matcha "backend")
    const re = new RegExp(`\\b${s.replace(/[-/\\^$*+?.()|[\]{}]/g, "\\$&")}\\b`);
    if (re.test(text)) return true;
  }
  return false;
}

type Category = "build" | "test" | "lint" | "db" | "dev" | "altro";
const CATEGORY_LABEL: Record<Category, string> = {
  build: "Build & compile",
  test:  "Test",
  lint:  "Lint & format",
  db:    "Database",
  dev:   "Dev / serve",
  altro: "Altro",
};
const CATEGORY_ICON: Record<Category, string> = {
  build: "🔨", test: "🧪", lint: "✨", db: "🗄️", dev: "▶", altro: "⚡",
};

function categorize(c: { label: string; command?: string; args?: string[] }): Category {
  const haystack = `${c.label} ${c.command ?? ""} ${(c.args ?? []).join(" ")}`.toLowerCase();
  if (/\b(test|spec|jest|vitest|playwright|pytest|cargo test|dotnet test|go test)\b/.test(haystack)) return "test";
  if (/\b(lint|format|fmt|prettier|eslint|stylelint|clippy|tsc|typecheck|check)\b/.test(haystack)) return "lint";
  if (/\b(build|compile|bundle|tsc --build|webpack|rollup|cargo build)\b/.test(haystack)) return "build";
  if (/\b(migrate|seed|ef database|sqlx|alembic|prisma|drizzle|db[-_:](?:up|init|reset|migrate|seed))\b/.test(haystack)) return "db";
  if (/\b(dev|start|serve|preview|watch|hmr|run dev|run start)\b/.test(haystack)) return "dev";
  return "altro";
}

function RunDebugView({
  tc,
  project,
  runConfigs,
  onRunConfigsChange,
  onLaunchConfig,
}: {
  tc: ReturnType<typeof useThemeColors>;
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

  // Polling lieve dei servizi systemd del progetto: serve solo a sapere quali
  // run config nascondere perché coperti da un servizio persistente.
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
    const t = window.setInterval(refresh, 8000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [projectId]);

  // Polling porte rilevate per link "Apri" (molto leggero).
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
    const t = window.setInterval(refresh, 10000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [projectId]);

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

function ViewHeader({
  title,
  subtitle,
  actions,
}: {
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
}) {
  const tc = useThemeColors();
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        padding: "6px 10px",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
      }}
    >
      <div>
        <div
          style={{
            fontSize: 12,
            fontWeight: 700,
            color: tc.text,
            textTransform: "uppercase",
            letterSpacing: "0.06em",
          }}
        >
          {title}
        </div>
        {subtitle && (
          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
            {subtitle}
          </div>
        )}
      </div>
      {actions}
    </div>
  );
}

export function SidebarManager({
  activeSidebarView,
  project,
  treeNodes,
  git,
  activeEditorTab,
  allOpenTabs,
  currentBranch,
  runConfigs,
  onRunConfigsChange,
  onLaunchConfig,
  searchQuery,
  searchBusy,
  searchResults,
  onSetSearchQuery,
  onSearch,
  onOpenFile,
  onSaveActive,
  onCreateEntry,
  onRefreshProject,
  onProjectAnalyzed,
  onSendToChat,
}: SidebarManagerProps) {
  const tc = useThemeColors();

  const renderOpenEditors = () => (
    <div style={{ borderBottom: `1px solid ${tc.border}` }}>
      <ViewHeader
        title="Open Editors"
        subtitle={`${allOpenTabs.length} file`}
        actions={
          <button
            type="button"
            onClick={onSaveActive}
            disabled={!activeEditorTab || !activeEditorTab.dirty || !project?.canWrite}
            title="Salva editor attivo"
            aria-label="Salva editor attivo"
            style={iconButton(tc, !activeEditorTab || !activeEditorTab.dirty || !project?.canWrite)}
          >
            💾
          </button>
        }
      />
      <div style={{ display: "flex", flexDirection: "column", gap: 4, padding: 8 }}>
        {allOpenTabs.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>
            Nessun editor aperto.
          </div>
        ) : (
          allOpenTabs.map((tab) => {
            // Fix open-editors: mostra il path relativo alla root del progetto
            // attivo, full path solo nel title (hover). Aiuta a vedere subito
            // il file di interesse senza spazio sprecato dal prefisso assoluto.
            const isInProject = project?.rootPath
              ? tab.path.startsWith(project.rootPath)
              : false;
            const display = isInProject && project?.rootPath
              ? tab.path.slice(project.rootPath.length).replace(/^\//, "")
              : shortenAbsolutePath(tab.path, project?.rootPath ?? undefined);
            // Segnale visuale se il file appartiene a un altro progetto (raro,
            // ma capita dopo uno switch progetto se il tab e' rimasto aperto).
            const outsideProject = !isInProject && project?.rootPath;
            return (
              <button
                key={`open-${tab.path}`}
                onClick={() => onOpenFile(tab.path)}
                title={tab.path + (outsideProject ? " (fuori dal progetto attivo)" : "")}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                  width: "100%",
                  background: "transparent",
                  border: "none",
                  color: outsideProject ? tc.warning : tc.text,
                  cursor: "pointer",
                  padding: "5px 6px",
                  borderRadius: 6,
                  textAlign: "left",
                }}
              >
                <span
                  style={{
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {display}
                </span>
                {tab.dirty && <span style={{ color: tc.warning }}>●</span>}
              </button>
            );
          })
        )}
      </div>
    </div>
  );

  if (activeSidebarView === "explorer") {
    return (
      <>
        {renderOpenEditors()}
        <ViewHeader
          title="Explorer"
          subtitle={project?.rootPath ? shortenAbsolutePath(project.rootPath) : "Apri un progetto"}
          actions={
            <div style={{ display: "flex", gap: 6 }}>
              <button
                type="button"
                onClick={() => onCreateEntry("file")}
                title="Nuovo file"
                aria-label="Nuovo file"
                style={iconButton(tc, !project?.canWrite)}
              >
                📄
              </button>
              <button
                type="button"
                onClick={() => onCreateEntry("directory")}
                title="Nuova cartella"
                aria-label="Nuova cartella"
                style={iconButton(tc, !project?.canWrite)}
              >
                📁
              </button>
            </div>
          }
        />
        <div
          style={{
            flex: 1,
            minHeight: 0,
            overflow: "auto",
            padding: "8px 8px 12px",
          }}
        >
          <ProjectExplorer
            project={project}
            initialNodes={treeNodes}
            activeFilePath={activeEditorTab?.path ?? null}
            onOpenFile={async (path) => {
              onOpenFile(path);
            }}
          />
        </div>
      </>
    );
  }

  if (activeSidebarView === "search") {
    return (
      <>
        <ViewHeader title="Search" subtitle="Ricerca nel progetto" />
        <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}` }}>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              value={searchQuery}
              onChange={(e) => onSetSearchQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") onSearch();
              }}
              placeholder="Cerca testo nel workspace"
              style={inputStyle(tc)}
            />
            <button
              type="button"
              onClick={onSearch}
              title="Avvia ricerca"
              aria-label="Avvia ricerca"
              style={iconButton(tc, searchBusy || !searchQuery.trim())}
            >
              🔎
            </button>
          </div>
        </div>
        <div style={{ flex: 1, minHeight: 0, overflow: "auto", padding: 8 }}>
          {searchBusy ? (
            <div className="text-muted">Ricerca in corso...</div>
          ) : searchResults.length === 0 ? (
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              Nessun risultato. Inserisci un termine e avvia la ricerca.
            </div>
          ) : (
            searchResults.map((item) => (
              <button
                key={`${item.path}:${item.line}:${item.column}`}
                onClick={() => onOpenFile(item.path, item.line)}
                style={listRowButton(tc)}
              >
                <div style={{ color: tc.text, fontSize: 12 }}>
                  {item.path}:{item.line}
                </div>
                <div style={{ color: tc.textMuted, fontSize: 11 }}>
                  {item.preview}
                </div>
              </button>
            ))
          )}
        </div>
      </>
    );
  }

  if (activeSidebarView === "source-control") {
    return (
      <>
        <ViewHeader title="Source Control" subtitle={currentBranch} />
        <div
          style={{ flex: 1, minHeight: 0, overflowX: "hidden", overflowY: "auto", padding: 8, minWidth: 0 }}
        >
          <SourceControlPanel
            project={project}
            git={git}
            onRefresh={async () => { onRefreshProject(); }}
            onProjectAnalyzed={onProjectAnalyzed}
            onOpenFileAtLine={async (path, line) => {
              onOpenFile(path, line, "primary");
            }}
            onSendToChat={onSendToChat}
          />
        </div>
      </>
    );
  }

  if (activeSidebarView === "run") {
    return (
      <RunDebugView
        tc={tc}
        project={project}
        runConfigs={runConfigs}
        onRunConfigsChange={onRunConfigsChange}
        onLaunchConfig={onLaunchConfig}
      />
    );
  }

  if (activeSidebarView === "docs") {
    return (
      <DocumentsSidebar
        project={project}
        onSendToChat={onSendToChat}
        onOpenInEditor={(relativePath) => onOpenFile(relativePath)}
      />
    );
  }

  if (activeSidebarView === "project-db") {
    return (
      <div style={{ flex: 1, minHeight: 0, height: "100%", overflow: "hidden", display: "flex", flexDirection: "column" }}>
        <ProjectDbPanel project={project} />
      </div>
    );
  }

  // server-monitor
  if (activeSidebarView === "server-monitor") {
    return (
      <>
        <ViewHeader title="Monitor" subtitle="Risorse server · ogni 2s" />
        <div style={{ flex: 1, minHeight: 0, overflowY: "auto" }}>
          <ServerMonitorPanel />
        </div>
      </>
    );
  }

  return null;
}
