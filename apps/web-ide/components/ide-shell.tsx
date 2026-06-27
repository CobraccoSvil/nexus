"use client";

import type { DashboardSnapshot } from "../lib/dashboard";
import {
  createProjectEntry,
  deleteProjectEntry,
  getGitStatus,
  getHealth,
  getIndexStatus,
  triggerReindexStale,
  getOutputChannels,
  getOutputEvents,
  getMyProjects,
  getPlaywrightRuns,
  clearPlaywrightRuns,
  getGatewayProviders,
  getProjectFile,
  getProjectPorts,
  killPortProcess,
  getProjectProblems,
  getRunConfigs,
  getProviderModels,
  getWorkbenchState,
  openProject,
  analyzeProject,
  detectProjectServices,
  registerProject,
  renameProjectEntry,
  saveProjectFile,
  searchProject,
  updateWorkbenchState,
  type AITraceEvent,
  type EditorGroupState,
  type EditorTabState,
  type GitRepositoryState,
  type OutputChannel,
  type OutputEvent,
  type PlaywrightRunSummary,
  type PortEntry,
  type ProblemItem,
  type RunConfigItem,
  type UserProjectDetails,
  type UserProjectSummary,
  type WorkbenchLayoutMode,
  type WorkbenchState,
  type WorkspaceTreeNode,
} from "../lib/api-client";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import dynamic from "next/dynamic";
import { useResize } from "../hooks/use-resize";
import { useThemeColors } from "../lib/theme";
import { useGlobalDialog } from "./global-dialog-provider";
import { useMultiChat } from "../lib/use-multi-chat";
import { useProfiles, DEFAULT_PROFILE_ID } from "../lib/use-profiles";
import { NoteDetail } from "./knowledge/note-detail";
import type { SidebarView } from "./sidebar/sidebar-manager";
import type { PanelTab } from "./panels/bottom-panel-manager";
import {
  useProjectDispatcher,
  useProjectStore,
  selectPlaywrightRuns,
  selectPlaywrightConfigChangedAt,
  selectPorts,
  selectFilesRecent,
  selectGitStatus,
  selectProblemsBadge,
  selectRunConfigsChangedAt,
} from "../lib/project-dispatcher";
import { isBinaryDocPath } from "../lib/file-kind";
import { ACTION_AGENT_HINT } from "../lib/chat-prompts";
import {
  EMPTY_GROUPS,
  basename,
  hydrateGroups,
  iconButton,
  makeTab,
  normalizeWorkbenchState,
  type ProviderHealthState,
  type ProviderKey,
  type SecondarySidebarView,
} from "./shell/shell-helpers";
import { TopBar } from "./shell/top-bar";
import { ActivityBar } from "./shell/activity-bar";
import { StatusBar } from "./shell/status-bar";
import { FirstAnalysisOverlay } from "./shell/first-analysis-overlay";
import { ShellOverlays } from "./shell/shell-overlays";
import { BottomPanelHeader } from "./shell/bottom-panel-header";
import { RightViewTabs } from "./shell/panel-tabs";

// Dynamic imports per componenti pesanti IDE
const ChatPanel = dynamic(() => import("./chat-panel.lazy"), {
  loading: () => <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center" }} />,
  ssr: false,
});

const EditorArea = dynamic(() => import("./editor/editor-area.lazy"), {
  loading: () => <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center" }} />,
  ssr: false,
});

const SqlQueryPanel = dynamic(
  () => import("./sql/sql-query-panel").then((m) => m.SqlQueryPanel),
  {
    loading: () => (
      <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", fontSize: 12 }}>
        Caricamento pannello SQL…
      </div>
    ),
    ssr: false,
  },
);

const SidebarManager = dynamic(() => import("./sidebar/sidebar-manager.lazy"), {
  loading: () => <div style={{ width: 300 }} />,
  ssr: false,
});

const BottomPanelManager = dynamic(() => import("./panels/bottom-panel-manager.lazy"), {
  loading: () => <div style={{ height: 250 }} />,
  ssr: false,
});

const ProfileSelector = dynamic(() => import("./chat/profile-selector.lazy"), {
  loading: () => <div>Loading...</div>,
  ssr: false,
});

const ProfileEditor = dynamic(() => import("./chat/profile-editor.lazy"), {
  loading: () => <div>Loading...</div>,
  ssr: false,
});


export function IdeShell({ dashboard, initialProjectId }: { dashboard: DashboardSnapshot; initialProjectId?: string }) {
  const tc = useThemeColors();
  const { promptDialog, confirmDialog, alertDialog } = useGlobalDialog();
  // Polling client-side dello health: il prop `dashboard` è uno snapshot SSR
  // e non si aggiorna mai. Senza questo, i LED DB/Redis/Core restano congelati
  // sullo stato del primo render della pagina /ide. Il vecchio LED "Brain"
  // (brain_rest) e' stato rimosso: il brain Python e' stato eliminato e i suoi
  // endpoint vivono ora in mcp-core (neural_core).
  const [liveHealth, setLiveHealth] = useState<{ database: boolean; redis: boolean; neural_core: boolean; tools_grpc?: boolean }>(
    dashboard.health ?? { database: false, redis: false, neural_core: false, tools_grpc: false }
  );
  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const h = await getHealth();
        if (!cancelled) setLiveHealth(h.components);
      } catch {
        if (!cancelled) setLiveHealth({ database: false, redis: false, neural_core: false, tools_grpc: false });
      }
    };
    refresh();
    const id = window.setInterval(refresh, 10000);
    return () => { cancelled = true; window.clearInterval(id); };
  }, []);
  const [projects, setProjects] = useState<UserProjectSummary[]>([]);
  const [activeProject, setActiveProject] = useState<UserProjectDetails | null>(null);
  const [treeNodes, setTreeNodes] = useState<WorkspaceTreeNode[]>([]);
  const [gitState, setGitState] = useState<GitRepositoryState | null>(null);
  const [editorGroups, setEditorGroups] = useState<EditorGroupState[]>(EMPTY_GROUPS);
  const [activeEditorGroupId, setActiveEditorGroupId] = useState("primary");
  const [layoutMode, setLayoutMode] = useState<WorkbenchLayoutMode>("ai-center");
  const [primarySidebarVisible, setPrimarySidebarVisible] = useState(true);
  const [secondarySidebarVisible, setSecondarySidebarVisible] = useState(false);
  const [secondarySidebarView, setSecondarySidebarView] = useState<SecondarySidebarView>("ai-tools");
  const [bottomPanelVisible, setBottomPanelVisible] = useState(true);
  const [activeSidebarView, setActiveSidebarView] = useState<SidebarView>("explorer");
  const [activePanelTab, setActivePanelTab] = useState<PanelTab>("terminal");
  const [pendingChatMessage, setPendingChatMessage] = useState<string | undefined>(undefined);
  const [pendingAutoSend, setPendingAutoSend] = useState(false);
  const [pendingProviderHint, setPendingProviderHint] = useState<{ provider?: string; model?: string } | undefined>(undefined);
  /** Hint tipo agente per i prompt d'azione (error-fix) dai pannelli del bottom panel:
   *  propagato come `agentTypeHint` -> bypassa la disambiguazione d'intent A/B lato backend. */
  const [pendingAgentTypeHint, setPendingAgentTypeHint] = useState<string | undefined>(undefined);
  /** Per messaggi da pannelli diagnostic (debug, problemi, ecc.): un turno con agente + tool anche se la chat era in «Studio». */
  const [pendingExternalAutomation, setPendingExternalAutomation] = useState<
    "study" | "confirm" | "automatic" | undefined
  >(undefined);
  const [agentRunEndSignal, setAgentRunEndSignal] = useState(0);

  // Bridge globale `nexus:chat:send` -> chat composer.
  // Permette ad altri pannelli (es. project-db-panel "Crea database via agente")
  // di iniettare un prompt + auto-send senza dover passare per props/context.
  // Detail atteso: { content: string, autoSend?: boolean, automation?: "study"|"confirm"|"automatic" }
  useEffect(() => {
    const handler = (ev: Event) => {
      const ce = ev as CustomEvent<{ content?: string; autoSend?: boolean; automation?: "study" | "confirm" | "automatic" }>;
      const content = ce.detail?.content;
      if (!content || typeof content !== "string") return;
      setPendingChatMessage(content);
      setPendingAutoSend(ce.detail?.autoSend !== false);
      if (ce.detail?.automation) setPendingExternalAutomation(ce.detail.automation);
    };
    window.addEventListener("nexus:chat:send", handler);
    return () => window.removeEventListener("nexus:chat:send", handler);
  }, []);

  // Bridge globale `nexus:editor:open-file` -> apri file nell'editor.
  // Permette al markdown renderer della chat e al tool nexus_open_file_in_editor
  // (via SSE event) di aprire un file senza passare per props.
  // Detail atteso: { path: string, line?: number }
  // openFileInGroup e' definito piu' sotto: usiamo un ref per evitare TDZ.
  const openFileInGroupRef = useRef<(path: string, line?: number) => Promise<void> | void>(() => {});
  useEffect(() => {
    const handler = (ev: Event) => {
      const ce = ev as CustomEvent<{ path?: string; line?: number }>;
      const path = ce.detail?.path;
      if (!path || typeof path !== "string") return;
      void openFileInGroupRef.current(path, ce.detail?.line);
    };
    window.addEventListener("nexus:editor:open-file", handler);
    return () => window.removeEventListener("nexus:editor:open-file", handler);
  }, []);

  // Nota KB aperta nel pannello destro (Editor Workspace) invece che nella
  // stretta colonna sinistra. notes-tab/code-wiki-tab emettono nexus:note:open.
  const [openNoteId, setOpenNoteId] = useState<string | null>(null);
  useEffect(() => {
    const open = (ev: Event) => {
      const id = (ev as CustomEvent<{ noteId?: string }>).detail?.noteId;
      if (id) setOpenNoteId(String(id));
    };
    const close = () => setOpenNoteId(null);
    window.addEventListener("nexus:note:open", open);
    window.addEventListener("nexus:note:close", close);
    return () => {
      window.removeEventListener("nexus:note:open", open);
      window.removeEventListener("nexus:note:close", close);
    };
  }, []);

  // Bridge `nexus:kb:open-code-doc` -> apre la sidebar Knowledge, che ora
  // monta KnowledgeWorkspace (sistema wiki unificato, API /api/wiki/*).
  // Navigazione codice -> doc.
  useEffect(() => {
    const handler = () => setActiveSidebarView("knowledge");
    window.addEventListener("nexus:kb:open-code-doc", handler);
    return () => window.removeEventListener("nexus:kb:open-code-doc", handler);
  }, []);

  // Bridge globale `nexus:sql:open` -> apri pannello SQL nella colonna destra.
  // Permette al markdown renderer della chat (chip "Esegui" sui blocchi ```sql)
  // di aprire il pannello SQL e pre-compilare l'editor.
  // Detail atteso: { sql?: string, autoRun?: boolean }
  // Il componente SqlQueryPanel ascolta a sua volta `nexus:sql:set-content`
  // per ricevere il SQL: qui ci limitiamo a switchare la vista destra e
  // rilanciare l'evento con il contenuto.
  const [rightView, setRightView] = useState<"editor" | "sql">("editor");
  useEffect(() => {
    const handler = (ev: Event) => {
      const ce = ev as CustomEvent<{ sql?: string; autoRun?: boolean }>;
      setRightView("sql");
      // Defer per assicurare che il pannello sia montato prima di iniettare il SQL.
      const sql = typeof ce.detail?.sql === "string" ? ce.detail.sql : undefined;
      const autoRun = ce.detail?.autoRun === true;
      if (sql !== undefined) {
        setTimeout(() => {
          window.dispatchEvent(
            new CustomEvent("nexus:sql:set-content", { detail: { sql, autoRun } }),
          );
        }, 50);
      }
    };
    window.addEventListener("nexus:sql:open", handler);
    return () => window.removeEventListener("nexus:sql:open", handler);
  }, []);
  const [leftWidth, setLeftWidth] = useState(300);
  const [rightWidth, setRightWidth] = useState(430);
  const [bottomHeight, setBottomHeight] = useState(250);
  const [viewportWidth, setViewportWidth] = useState(1600);
  const [viewportHeight, setViewportHeight] = useState(900);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [projectBusy, setProjectBusy] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<
    Array<{ path: string; line: number; column: number; preview: string }>
  >([]);
  const [searchBusy, setSearchBusy] = useState(false);
  const [problemItems, setProblemItems] = useState<ProblemItem[]>([]);
  const [outputChannels, setOutputChannels] = useState<OutputChannel[]>([]);
  const [selectedOutputChannel, setSelectedOutputChannel] = useState("System");
  const [outputEvents, setOutputEvents] = useState<OutputEvent[]>([]);
  const [ports, setPorts] = useState<PortEntry[]>([]);
  const [playwrightRuns, setPlaywrightRuns] = useState<PlaywrightRunSummary[]>([]);
  const [playwrightConfigured, setPlaywrightConfigured] = useState(false);
  const [runConfigs, setRunConfigs] = useState<RunConfigItem[]>([]);
  const [providerStatus, setProviderStatus] = useState<Record<ProviderKey, ProviderHealthState>>({
    openai: { ok: null },
    anthropic: { ok: null },
    google: { ok: null },
    deepseek: { ok: null },
    mistral: { ok: null },
  });
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [workbenchReady, setWorkbenchReady] = useState(false);
  // Gate prima analisi: impedisce l'uso dell'IDE finche' il progetto non e' analizzato
  const [analysisInProgress, setAnalysisInProgress] = useState(false);
  const [analysisStep, setAnalysisStep] = useState("");
  const [pendingEditorFocus, setPendingEditorFocus] = useState<{ path: string; line: number } | null>(
    null,
  );
  const editorRefs = useRef<Record<string, HTMLTextAreaElement | null>>({});

  useEffect(() => {
    if (typeof window === "undefined") return;
    const onResize = () => { setViewportWidth(window.innerWidth); setViewportHeight(window.innerHeight); };
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const isNarrowViewport = viewportWidth < 1280;
  const isMobileViewport = viewportWidth < 980;
  const activityBarWidth = isMobileViewport ? 46 : 52;
  const activityButtonSize = isMobileViewport ? 32 : 36;
  const leftSidebarMinWidth = isMobileViewport ? 160 : isNarrowViewport ? 190 : 220;
  const leftSidebarMaxWidth = Math.max(
    leftSidebarMinWidth,
    Math.min(520, Math.floor(viewportWidth * 0.46)),
  );
  const rightSidebarMinWidth = isMobileViewport ? 240 : 280;
  const rightSidebarMaxWidth = Math.max(
    rightSidebarMinWidth,
    Math.min(620, Math.floor(viewportWidth * 0.6)),
  );
  const effectiveLeftWidth = Math.max(leftSidebarMinWidth, Math.min(leftSidebarMaxWidth, leftWidth));
  const effectiveRightWidth = Math.max(rightSidebarMinWidth, Math.min(rightSidebarMaxWidth, rightWidth));

  // Bottom panel: altezza proporzionale al viewport (min 120, max 50% dello spazio disponibile)
  const bottomPanelMinHeight = isMobileViewport ? 120 : 160;
  const bottomPanelMaxHeight = Math.max(bottomPanelMinHeight, Math.min(460, Math.floor((viewportHeight - 72) * 0.5)));

  useEffect(() => {
    setLeftWidth((current) => Math.max(leftSidebarMinWidth, Math.min(leftSidebarMaxWidth, current)));
    setRightWidth((current) => Math.max(rightSidebarMinWidth, Math.min(rightSidebarMaxWidth, current)));
    setBottomHeight((current) => Math.max(bottomPanelMinHeight, Math.min(bottomPanelMaxHeight, current)));
  }, [leftSidebarMaxWidth, leftSidebarMinWidth, rightSidebarMaxWidth, rightSidebarMinWidth, bottomPanelMinHeight, bottomPanelMaxHeight]);

  const resizeLeft = useResize({
    direction: "horizontal",
    onDelta: useCallback((delta: number) => {
      setLeftWidth((current) => Math.max(leftSidebarMinWidth, Math.min(leftSidebarMaxWidth, current + delta)));
    }, [leftSidebarMaxWidth, leftSidebarMinWidth]),
  });
  const resizeBottom = useResize({
    direction: "vertical",
    onDelta: useCallback((delta: number) => {
      setBottomHeight((current) => Math.max(bottomPanelMinHeight, Math.min(bottomPanelMaxHeight, current - delta)));
    }, [bottomPanelMinHeight, bottomPanelMaxHeight]),
  });
  const resizeRight = useResize({
    direction: "horizontal",
    onDelta: useCallback((delta: number) => {
      setRightWidth((current) => Math.max(rightSidebarMinWidth, Math.min(rightSidebarMaxWidth, current - delta)));
    }, [rightSidebarMaxWidth, rightSidebarMinWidth]),
  });
  const resizeCenter = useResize({
    direction: "horizontal",
    onDelta: useCallback((delta: number) => {
      setRightWidth((current) => Math.max(rightSidebarMinWidth, Math.min(rightSidebarMaxWidth, current - delta)));
    }, [rightSidebarMaxWidth, rightSidebarMinWidth]),
  });

  const multiChat = useMultiChat(activeProject?.id ?? "default");

  // Audit 27/05/2026: handler memoizzati per evitare loop "Maximum update depth
  // exceeded" in ChatPanel. Prima erano definiti inline come `(ratio) =>
  // multiChat.setCtxRatio(...)`, ricreati ad ogni render di IdeShell. Questo
  // faceva si che il useEffect in chat-panel.tsx:378 (deps include
  // onCtxRatioChange) si rilanciasse ad ogni render, chiamasse setCtxRatio,
  // cambiasse lo state di useMultiChat, causasse re-render di IdeShell, e
  // ricreasse l'handler -> loop infinito. Stessa cosa per onAgentActivityChange.
  // I setter di useMultiChat (setAgentActive, setCtxRatio) sono gia' useCallback
  // stabili, quindi questi useRef "indiretti" puntano sempre alla versione
  // corrente di multiChat.activeTabId/setters.
  const multiChatRef = useRef(multiChat);
  multiChatRef.current = multiChat;
  const handleChatAgentActivityChange = useCallback((active: boolean) => {
    const mc = multiChatRef.current;
    if (mc.activeTabId) mc.setAgentActive(mc.activeTabId, active);
  }, []);
  const handleChatCtxRatioChange = useCallback((ratio: number | null) => {
    const mc = multiChatRef.current;
    if (mc.activeTabId) mc.setCtxRatio(mc.activeTabId, ratio);
  }, []);
  // Anche onRunEnd era inline: il useEffect in chat-panel.tsx:391 con deps
  // [agentRun, onRunEnd] ri-lanciava l'effect ad ogni render di IdeShell,
  // amplificando la cascata di re-render. Memoizzato con lo stesso pattern.
  // refreshProviderStatus e' definito piu' sotto (useCallback), usiamo un ref
  // per evitare il TDZ (Temporal Dead Zone) delle const.
  const activeProjectRef = useRef(activeProject);
  activeProjectRef.current = activeProject;
  const refreshProviderStatusRef = useRef<() => Promise<void>>(async () => {});
  const handleRunEnd = useCallback((run: { provider: string; model: string; status: string }) => {
    const delay = run.status === "failed" ? 500 : 2000;
    void new Promise<void>((resolve) => setTimeout(resolve, delay))
      .then(() => void refreshProviderStatusRef.current());
    setAgentRunEndSignal((n) => n + 1);
    // Un turno appena concluso puo' aver generato/aggiornato un documento via
    // nexus_doc_generate. Notifichiamo il pannello DOCUMENTI (se montato) di
    // rifare la fetch: senza questo segnale resta vuoto finche' non lo si
    // riapre. Disaccoppiato via window event (DocumentsSidebar lo ascolta).
    if (typeof window !== "undefined") {
      window.dispatchEvent(new CustomEvent("nexus:documents:refresh"));
    }
    const proj = activeProjectRef.current;
    if (proj) {
      void getPlaywrightRuns(proj.id)
        .then((res) => {
          setPlaywrightRuns(res.runs ?? []);
          setPlaywrightConfigured(res.configured ?? false);
        })
        .catch(() => { /* ignora */ });
    }
  }, []);
  const handleExternalInputConsumed = useCallback(() => {
    setPendingChatMessage(undefined);
    setPendingAutoSend(false);
    setPendingProviderHint(undefined);
    setPendingAgentTypeHint(undefined);
    setPendingExternalAutomation(undefined);
  }, []);

  // Dispatcher centrale: connessione SSE unica per progetto, alimenta lo store
  // Zustand da cui i pannelli leggono in tempo reale (eliminando il polling 4-8s).
  useProjectDispatcher(activeProject?.id);
  // Lo store del dispatcher e' la NUOVA fonte di verita' per i pannelli.
  // Per ora `playwrightRunsFromDispatcher` viene mergeato con `playwrightRuns`
  // (state legacy) per compatibilita' durante la migrazione. Il polling esistente
  // (useEffect linee ~898 e ~943) verra' rimosso in una fase successiva.
  const playwrightRunsFromDispatcher = useProjectStore(selectPlaywrightRuns);
  const playwrightConfigChangedAt = useProjectStore(selectPlaywrightConfigChangedAt);
  const portsFromDispatcher = useProjectStore(selectPorts);
  const filesRecentFromDispatcher = useProjectStore(selectFilesRecent);

  // Auto-refresh pannello Playwright quando il dispatcher rileva playwright.config.*
  useEffect(() => {
    if (playwrightConfigChangedAt > 0 && activeProject) {
      void getPlaywrightRuns(activeProject.id)
        .then((res) => {
          setPlaywrightRuns(res.runs ?? []);
          setPlaywrightConfigured(res.configured ?? false);
        })
        .catch(() => { /* ignora */ });
    }
  }, [playwrightConfigChangedAt, activeProject]);

  // Auto-refresh problemItems quando arriva FindingsUpdated dal dispatcher.
  // Il badge usa il valore "live" dal dispatcher (selectProblemsBadge), ma per
  // la lista completa serve refetch via API perche' l'evento non contiene items.
  const problemsBadgeFromDispatcher = useProjectStore(selectProblemsBadge);
  useEffect(() => {
    if (!activeProject) return;
    if (problemsBadgeFromDispatcher === 0) return; // skip stato iniziale
    void getProjectProblems(activeProject.id)
      .then((res) => setProblemItems(res.items ?? []))
      .catch(() => { /* ignora */ });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [problemsBadgeFromDispatcher]);

  // Auto-refresh gitState quando arriva GitStatusChanged dal dispatcher.
  // Il payload e' magro (branch, ahead, behind, modified_count) ma noi
  // serviamo GitRepositoryState completo (staged/unstaged/untracked file list),
  // quindi usiamo l'evento come trigger di refresh full via API.
  const gitStatusFromDispatcher = useProjectStore(selectGitStatus);
  useEffect(() => {
    if (!activeProject) return;
    if (!gitStatusFromDispatcher.branch) return; // skip stato iniziale
    void getGitStatus(activeProject.id)
      .then((res) => setGitState(res.git))
      .catch(() => { /* ignora */ });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gitStatusFromDispatcher]);

  const [chatProvider, setChatProvider] = useState<string>("auto");
  const [chatModel, setChatModel] = useState<string>("auto");
  const [chatAutomationMode, setChatAutomationMode] = useState<"study" | "confirm" | "automatic">("confirm");
  const [chatSupervisorMode, setChatSupervisorMode] = useState<"none" | "anomaly" | "interleaved" | "continuous">("none");
  // Ripristina da localStorage dopo l'idratazione (SSR-safe)
  useEffect(() => {
    try {
      const p = localStorage.getItem("nexus:chatProvider");
      const m = localStorage.getItem("nexus:chatModel");
      const a = localStorage.getItem("nexus:chatAutomationMode") as "study" | "confirm" | "automatic" | null;
      const s = localStorage.getItem("nexus:chatSupervisorMode") as "none" | "anomaly" | "interleaved" | "continuous" | null;
      // Accetta solo valori validi — resetta a "auto" se era rimasto un provider fisso
      const validProviders = ["auto", "anthropic", "openai", "google"];
      if (p && validProviders.includes(p)) setChatProvider(p); else localStorage.removeItem("nexus:chatProvider");
      if (m) setChatModel(m);
      if (a) setChatAutomationMode(a);
      if (s) setChatSupervisorMode(s);
    } catch {}
  }, []);
  const [showMemory, setShowMemory] = useState(false);
  const [chatProviderModels, setChatProviderModels] = useState<string[]>([]);
  const [aiTraces, setAiTraces] = useState<AITraceEvent[]>([]);

  // Profili
  const profilesMgr = useProfiles();
  const [selectedProfileId, setSelectedProfileId] = useState<string>("auto");
  const [showProfileEditor, setShowProfileEditor] = useState(false);
  const [editingProfile, setEditingProfile] = useState<import("../lib/api-client").UserProfile | undefined>(undefined);

  useEffect(() => {
    if (chatProvider === "auto") {
      setChatProviderModels([]);
      setChatModel("auto");
      return;
    }
    let cancelled = false;
    void getProviderModels(chatProvider)
      .then((response) => {
        if (cancelled) return;
        setChatProviderModels(response.models ?? []);
        setChatModel((current) =>
          current !== "auto" && response.models.includes(current) ? current : "auto",
        );
      })
      .catch(() => {
        if (cancelled) return;
        setChatProviderModels([]);
        setChatModel("auto");
      });
    return () => { cancelled = true; };
  }, [chatProvider]);

  const allOpenTabs = useMemo(
    () => editorGroups.flatMap((group) => group.tabs),
    [editorGroups],
  );
  const allOpenPaths = useMemo(
    () => [...new Set(allOpenTabs.map((tab) => tab.path))],
    [allOpenTabs],
  );
  const currentBranch = activeProject?.currentBranch || gitState?.currentBranch || "n/a";
  const showSecondaryAi = layoutMode === "editor-center" && secondarySidebarVisible;
  const activeGroup =
    editorGroups.find((group) => group.id === activeEditorGroupId) ?? editorGroups[0];
  const activeEditorTab =
    activeGroup?.tabs.find((tab) => tab.path === activeGroup.activePath) ?? null;
  // Preferiamo il badge dal dispatcher (zero-latency, aggiornato live da
  // FindingsUpdated) rispetto a problemItems.length che si aggiorna solo
  // dopo che il refresh API completa.
  const problemCount = problemsBadgeFromDispatcher > 0
    ? problemsBadgeFromDispatcher
    : problemItems.length;

  const cycleLayoutMode = useCallback(() => {
    setLayoutMode((current) => {
      if (current === "ai-center") return "split-ai-editor";
      if (current === "split-ai-editor") return "editor-center";
      return "ai-center";
    });
  }, []);

  const toggleFullscreen = useCallback(async () => {
    if (typeof document === "undefined") return;
    try {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
      } else {
        await document.documentElement.requestFullscreen();
      }
    } catch {
      // Ignore browser-level fullscreen errors.
    }
  }, []);

  const refreshOperationalViews = useCallback(
    async (projectId: string) => {
      // Promise.allSettled: se UNO degli endpoint fallisce (es. backend
      // momentaneamente non risponde su /run-configs), gli altri 4 si
      // applicano comunque. Prima invece bastava 1 errore per perdere TUTTE
      // le viste e mostrare "Impossibile caricare le viste operative"
      // bloccante anche quando il problema era gia' risolto.
      const results = await Promise.allSettled([
        getProjectProblems(projectId),
        getOutputChannels(projectId),
        getProjectPorts(projectId),
        getPlaywrightRuns(projectId),
        getRunConfigs(projectId),
      ]);
      const [problemsR, channelsR, portsR, playwrightR, runConfigsR] = results;
      if (problemsR.status === "fulfilled") setProblemItems(problemsR.value.items ?? []);
      if (channelsR.status === "fulfilled") {
        setOutputChannels(channelsR.value.channels ?? []);
        const firstChannel = channelsR.value.channels?.[0]?.id ?? "System";
        setSelectedOutputChannel((current) => current || firstChannel);
      }
      if (portsR.status === "fulfilled") setPorts(portsR.value.ports ?? []);
      if (playwrightR.status === "fulfilled") {
        setPlaywrightRuns(playwrightR.value.runs ?? []);
        setPlaywrightConfigured(playwrightR.value.configured ?? false);
      }
      if (runConfigsR.status === "fulfilled") setRunConfigs(runConfigsR.value.configs ?? []);

      // Se TUTTI hanno fallito: errore reale (backend down).
      // Se almeno uno ha avuto successo: reset dello stato d'errore (su un
      // refresh successivo le viste sono di nuovo aggiornate).
      const allFailed = results.every((r) => r.status === "rejected");
      if (allFailed) {
        const firstError = results.find((r) => r.status === "rejected") as PromiseRejectedResult | undefined;
        const msg = firstError?.reason instanceof Error
          ? firstError.reason.message
          : "Impossibile caricare le viste operative.";
        setProjectError(msg);
      } else {
        // Almeno qualcosa funziona: pulisci eventuale errore stale
        setProjectError((current) =>
          current === "Impossibile caricare le viste operative." ? null : current,
        );
      }
    },
    [],
  );

  // Quando il dispatcher segnala file modificati (dall'agente), triggera un
  // refresh dei pannelli operativi (problemi, porte, playwright) per allineare
  // l'UI in tempo reale senza aspettare il polling.
  const filesRefreshRef = useRef(0);
  useEffect(() => {
    if (!activeProject?.id || filesRecentFromDispatcher.length === 0) return;
    // Evita il refresh al primo mount (solo sui cambiamenti successivi)
    const count = filesRecentFromDispatcher.length;
    if (filesRefreshRef.current === 0) {
      filesRefreshRef.current = count;
      return;
    }
    if (count !== filesRefreshRef.current) {
      filesRefreshRef.current = count;
      void refreshOperationalViews(activeProject.id);
      // Anche il file tree va rinfrescato: openProject ritorna tree fresh.
      // Senza questo, la sidebar non mostra file nuovi creati dall'agente.
      void openProject(activeProject.id)
        .then((opened) => setTreeNodes(opened.tree))
        .catch(() => { /* ignora */ });
    }
  }, [filesRecentFromDispatcher.length, activeProject?.id, refreshOperationalViews]);

  const loadOutputEvents = useCallback(
    async (projectId: string, channel: string) => {
      try {
        const response = await getOutputEvents(projectId, channel);
        const all = response.events ?? [];
        const cutoff = clearTimestamps.current[channel] ?? 0;
        const filtered = cutoff > 0
          ? all.filter((e) => new Date(e.createdAt).getTime() >= cutoff)
          : all;
        setOutputEvents(filtered);
      } catch {
        setOutputEvents([]);
      }
    },
    [],
  );

  const applyWorkbench = useCallback(
    async (
      project: UserProjectDetails,
      state: Partial<WorkbenchState>,
      sessionActivePaths: string[],
    ) => {
      const normalized = normalizeWorkbenchState(state);
      const hydratedGroups = await hydrateGroups(project.id, normalized, sessionActivePaths, getProjectFile);
      setLayoutMode(normalized.layoutMode);
      setPrimarySidebarVisible(normalized.primarySidebarVisible);
      setSecondarySidebarVisible(Boolean(normalized.secondarySidebarVisible));
      setSecondarySidebarView((normalized.secondarySidebarView as SecondarySidebarView) ?? "ai-tools");
      setBottomPanelVisible(normalized.bottomPanelVisible);
      setActiveSidebarView(normalized.activeSidebarView as SidebarView);
      setActivePanelTab(normalized.activePanelTab as PanelTab);
      setLeftWidth(normalized.leftWidth);
      setRightWidth(normalized.rightWidth);
      setBottomHeight(normalized.bottomHeight);
      setEditorGroups(hydratedGroups);
      setActiveEditorGroupId(hydratedGroups[0]?.id ?? "primary");
      setWorkbenchReady(true);
    },
    [],
  );

  // Esegue la prima analisi del progetto (struttura, linguaggi, servizi)
  const runFirstAnalysis = useCallback(
    async (projectId: string) => {
      setAnalysisInProgress(true);
      try {
        setAnalysisStep("Analisi struttura progetto...");
        await analyzeProject(projectId);
        setAnalysisStep("Rilevamento servizi e configurazioni...");
        await detectProjectServices(projectId).catch(() => {/* opzionale */});
        setAnalysisStep("Completamento...");
        // Ricarica il progetto con i dati aggiornati
        const refreshed = await openProject(projectId);
        setActiveProject(refreshed.project);
        setTreeNodes(refreshed.tree);
        setGitState(refreshed.git);
        // Aggiorna la lista progetti
        const projectsResponse = await getMyProjects();
        setProjects(projectsResponse.projects);
      } catch (err) {
        console.error("[first-analysis] errore:", err);
        // Non bloccare: anche se l'analisi fallisce, l'utente puo' usare l'IDE
      } finally {
        setAnalysisInProgress(false);
        setAnalysisStep("");
      }
    },
    [],
  );

  const handleOpenProject = useCallback(
    async (projectId: string, refreshList = true) => {
      setProjectBusy(true);
      setWorkbenchReady(false);
      setProjectError(null);
      try {
        const [openResponse, workbenchResponse] = await Promise.all([
          openProject(projectId),
          getWorkbenchState(projectId),
        ]);

        setActiveProject(openResponse.project);
        setTreeNodes(openResponse.tree);
        setGitState(openResponse.git);
        // Aggiorna indice vettoriale in background per file modificati
        void getIndexStatus(projectId).then((status) => {
          if (status.staleCount > 0) {
            void triggerReindexStale(projectId).then((res) => {
              if (res.reindexed > 0) {
                console.info(`[index] re-indicizzati ${res.reindexed} file (${res.skipped} invariati)`);
              }
            }).catch(() => {/* silent */});
          }
        }).catch(() => {/* silent */});
        await applyWorkbench(
          openResponse.project,
          workbenchResponse.state ?? {},
          workbenchResponse.session?.activeFilePaths ?? [],
        );
        await refreshOperationalViews(projectId);
        if (refreshList) {
          const projectsResponse = await getMyProjects();
          setProjects(projectsResponse.projects);
        }
        // Se il progetto non e' mai stato analizzato, avvia la prima analisi automatica
        if (!openResponse.project.isAnalyzed) {
          void runFirstAnalysis(projectId);
        }
      } catch (error) {
        setProjectError(error instanceof Error ? error.message : "Impossibile aprire il progetto.");
      } finally {
        setProjectBusy(false);
      }
    },
    [applyWorkbench, refreshOperationalViews, runFirstAnalysis],
  );

  useEffect(() => {
    let cancelled = false;

    const loadProjects = async () => {
      try {
        const response = await getMyProjects();
        if (cancelled) return;
        setProjects(response.projects);
        const preferredProject = initialProjectId
          ? response.projects.find((project) => project.id === initialProjectId)
          : undefined;
        const fallbackProject =
          response.projects.find((project) => project.lastOpenedAt) ?? response.projects[0];
        const projectToOpen = preferredProject ?? fallbackProject;
        if (projectToOpen) {
          await handleOpenProject(projectToOpen.id, false);
        }
      } catch (error) {
        if (!cancelled) {
          setProjectError(error instanceof Error ? error.message : "Impossibile caricare i progetti.");
        }
      }
    };

    void loadProjects();
    return () => {
      cancelled = true;
    };
    // initialProjectId e' una prop stabile: vogliamo che il bootstrap di
    // progetti giri una volta sola al mount + quando handleOpenProject cambia.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handleOpenProject]);

  const refreshProviderStatus = useCallback(async () => {
    try {
      // Usa il gateway come fonte autoritativa (ha le chiavi caricate dal DB).
      // Il backend mcp-core arricchisce ogni entry con `cooldown_seconds_remaining`
      // se il provider è in cooldown (errore credit/quota detectato).
      const data = await getGatewayProviders();
      type GwEntry = {
        name: string;
        // null = gateway offline, stato dall'ultimo health probe o mai misurato
        healthy: boolean | null;
        configured?: boolean;
        error?: string;
        cooldown_seconds_remaining?: number;
        last_health_check_at?: string;
      };
      const gwList = (data?.providers as GwEntry[]) ?? [];
      const resolve = (key: ProviderKey): ProviderHealthState => {
        const gw = gwList.find((p) => p.name === key);
        if (!gw) return { ok: null };
        if (gw.cooldown_seconds_remaining && gw.cooldown_seconds_remaining > 0) {
          // Provider in cooldown (es. credit too low): pallino giallo (billing)
          // con motivo + tempo rimanente nel tooltip.
          const mins = Math.round(gw.cooldown_seconds_remaining / 60);
          const hours = Math.round(gw.cooldown_seconds_remaining / 3600);
          const remaining = gw.cooldown_seconds_remaining > 3600
            ? `~${hours}h`
            : `~${mins}min`;
          return {
            ok: false,
            billing: true,
            reason: `${gw.error ?? "In cooldown"} (${remaining} rimanenti). Nexus userà automaticamente un altro provider.`,
          };
        }
        // healthy null = gateway offline ma il provider era in lista (stato storico
        // dall'health probe o provider mai testato). Mostriamo grigio (ok: null)
        // invece di rosso per non allarmare su uno stato non aggiornato.
        if (gw.healthy === null || gw.healthy === undefined) {
          return { ok: null };
        }
        return gw.healthy
          ? { ok: true }
          : { ok: false, reason: gw.error ?? "Provider non disponibile" };
      };
      // Short-circuit: evita re-render se lo stato non e' cambiato (il polling
      // ogni 15s non deve causare cascate di render inutili).
      const next = {
        openai: resolve("openai"),
        anthropic: resolve("anthropic"),
        google: resolve("google"),
        deepseek: resolve("deepseek"),
        mistral: resolve("mistral"),
      };
      setProviderStatus((prev) => {
        const keys: ProviderKey[] = ["openai", "anthropic", "google", "deepseek", "mistral"];
        const changed = keys.some((k) =>
          prev[k].ok !== next[k].ok ||
          prev[k].billing !== next[k].billing ||
          prev[k].reason !== next[k].reason,
        );
        return changed ? next : prev;
      });
    } catch {
      // Gateway non raggiungibile: mantieni stato sconosciuto (solo se diverso)
      setProviderStatus((prev) => {
        const keys: ProviderKey[] = ["openai", "anthropic", "google", "deepseek", "mistral"];
        const allNull = keys.every((k) => prev[k].ok === null && !prev[k].billing && !prev[k].reason);
        return allNull ? prev : {
          openai: { ok: null }, anthropic: { ok: null }, google: { ok: null },
          deepseek: { ok: null }, mistral: { ok: null },
        };
      });
    }
  }, []);
  refreshProviderStatusRef.current = refreshProviderStatus;

  useEffect(() => {
    void refreshProviderStatus();
    // Polling rapido (15s) così quando un provider va in cooldown per errore
    // (es. credit too low) il LED giallo compare quasi subito senza dover
    // ricaricare la pagina.
    const timer = window.setInterval(() => {
      void refreshProviderStatus();
    }, 15000);
    return () => window.clearInterval(timer);
  }, [refreshProviderStatus]);

  // Ref per leggere outputChannels corrente senza includerlo nelle dep (evita loop)
  const outputChannelsRef = useRef<OutputChannel[]>([]);
  outputChannelsRef.current = outputChannels;
  const activePanelTabRef = useRef<PanelTab>("terminal");
  activePanelTabRef.current = activePanelTab;
  const selectedOutputChannelRef = useRef("System");
  selectedOutputChannelRef.current = selectedOutputChannel;

  // Timestamp (ms epoch) dell'ultimo clear per canale → persiste in sessionStorage
  // in modo che il filtro sopravviva ai cambi di tab e ai remount del pannello.
  const clearTimestamps = useRef<Record<string, number>>({});

  useEffect(() => {
    if (typeof sessionStorage === "undefined") return;
    try {
      const stored = sessionStorage.getItem("nexus:clearTimestamps");
      if (stored) clearTimestamps.current = JSON.parse(stored) as Record<string, number>;
    } catch { /* ignora */ }
  }, []);

  // Polling canali output (non ancora cablato al dispatcher). Le porte sono
  // ora gestite dal dispatcher: vedi `portsFromDispatcher`. Intervallo: 10s
  // se nessun processo attivo, 4s con processi attivi (per stream live dei log).
  useEffect(() => {
    if (!activeProject) return;
    const tick = async () => {
      try {
        const channelsRes = await getOutputChannels(activeProject.id);
        setOutputChannels(channelsRes.channels ?? []);
        if (activePanelTabRef.current === "output" && selectedOutputChannelRef.current) {
          void loadOutputEvents(activeProject.id, selectedOutputChannelRef.current);
        }
      } catch { /* ignora */ }
    };
    const getInterval = () =>
      outputChannelsRef.current.some((ch) => ch.label?.startsWith("●")) ? 4000 : 10000;
    let timer = window.setInterval(tick, getInterval());
    const adjustTimer = () => {
      window.clearInterval(timer);
      timer = window.setInterval(tick, getInterval());
    };
    const adjTimer = window.setInterval(adjustTimer, 5000);
    return () => { window.clearInterval(timer); window.clearInterval(adjTimer); };
  }, [activeProject, loadOutputEvents]);

  useEffect(() => {
    if (typeof document === "undefined") return;
    const onFullscreenChange = () => {
      setIsFullscreen(Boolean(document.fullscreenElement));
    };
    onFullscreenChange();
    document.addEventListener("fullscreenchange", onFullscreenChange);
    return () => {
      document.removeEventListener("fullscreenchange", onFullscreenChange);
    };
  }, []);

  useEffect(() => {
    if (!activeProject || !selectedOutputChannel) return;
    void loadOutputEvents(activeProject.id, selectedOutputChannel);
    // activeProject viene letto solo per .id (gia' in deps come activeProject?.id).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProject?.id, selectedOutputChannel, loadOutputEvents]);

  // Polling Fix M45 RIMOSSO: ora i pannelli operativi (porte, playwright,
  // problemi) si aggiornano in tempo reale via dispatcher SSE — vedi
  // `useProjectDispatcher(activeProject?.id)` sopra. Resta solo un refresh
  // Auto-refresh run configs via dispatcher SSE (RunConfigChanged).
  // Fallback polling 120s per sicurezza (es. modifica diretta DB).
  const runConfigsChangedAt = useProjectStore(selectRunConfigsChangedAt);
  useEffect(() => {
    const projectId = activeProject?.id;
    if (!projectId || runConfigsChangedAt === 0) return;
    const refresh = async () => {
      try {
        const runConfigsRes = await getRunConfigs(projectId);
        setRunConfigs(runConfigsRes.configs ?? []);
      } catch {
        /* best-effort */
      }
    };
    void refresh();
  }, [activeProject?.id, runConfigsChangedAt]);

  useEffect(() => {
    const projectId = activeProject?.id;
    if (!projectId) return;
    const refresh = async () => {
      try {
        const runConfigsRes = await getRunConfigs(projectId);
        setRunConfigs(runConfigsRes.configs ?? []);
      } catch {
        /* best-effort */
      }
    };
    const interval = window.setInterval(refresh, 120_000);
    return () => window.clearInterval(interval);
  }, [activeProject?.id]);

  useEffect(() => {
    if (!activeProject || !workbenchReady) return;
    const timeout = window.setTimeout(() => {
      void updateWorkbenchState(
        activeProject.id,
        {
          layoutMode,
          primarySidebarVisible,
          secondarySidebarVisible,
          secondarySidebarView,
          layoutControlStyle: "icon-menu",
          iconButtonsOnly: true,
          bottomPanelVisible,
          activeSidebarView,
          activePanelTab,
          leftWidth,
          rightWidth,
          bottomHeight,
          editorGroups,
          ai: {
            activeContextPaths: allOpenPaths,
          },
          terminal: {
            activeTabId: "shell-1",
            tabs: [{ id: "shell-1", title: "shell 1" }],
          },
          chat: {
            provider: chatProvider,
            model: chatModel,
            automationMode: chatAutomationMode,
          },
        },
        allOpenPaths,
        activeProject.rootPath,
      );
    }, 500);

    return () => {
      window.clearTimeout(timeout);
    };
  }, [
    activeProject,
    activePanelTab,
    activeSidebarView,
    allOpenPaths,
    bottomHeight,
    bottomPanelVisible,
    secondarySidebarVisible,
    secondarySidebarView,
    editorGroups,
    layoutMode,
    leftWidth,
    primarySidebarVisible,
    rightWidth,
    workbenchReady,
    chatAutomationMode,
    chatModel,
    chatProvider,
  ]);

  useEffect(() => {
    if (layoutMode === "editor-center" && !secondarySidebarVisible) {
      setSecondarySidebarVisible(true);
      setSecondarySidebarView("ai-tools");
    }
    if (layoutMode !== "editor-center" && secondarySidebarVisible) {
      setSecondarySidebarVisible(false);
    }
  }, [layoutMode, secondarySidebarVisible]);

  useEffect(() => {
    if (!pendingEditorFocus) return;
    const owningGroup = editorGroups.find((group) =>
      group.tabs.some((tab) => tab.path === pendingEditorFocus.path),
    );
    if (!owningGroup) return;
    const textarea = editorRefs.current[owningGroup.id];
    const targetTab = owningGroup.tabs.find((tab) => tab.path === pendingEditorFocus.path);
    if (!textarea || !targetTab) return;
    const targetContent = targetTab.content ?? "";

    let offset = 0;
    let currentLine = 1;
    while (offset < targetContent.length && currentLine < pendingEditorFocus.line) {
      if (targetContent[offset] === "\n") {
        currentLine += 1;
      }
      offset += 1;
    }

    textarea.focus();
    textarea.setSelectionRange(offset, offset);
    textarea.scrollTop = Math.max(0, (pendingEditorFocus.line - 3) * 20);
    setPendingEditorFocus(null);
  }, [editorGroups, pendingEditorFocus]);

  const refreshProject = useCallback(async () => {
    if (!activeProject) return;
    try {
      const [opened, gitResponse] = await Promise.all([
        openProject(activeProject.id),
        getGitStatus(activeProject.id),
      ]);
      setActiveProject(opened.project);
      setTreeNodes(opened.tree);
      setGitState(gitResponse.git);
      await refreshOperationalViews(activeProject.id);
      await loadOutputEvents(activeProject.id, selectedOutputChannel);
    } catch (error) {
      setProjectError(error instanceof Error ? error.message : "Impossibile aggiornare il progetto.");
    }
  }, [activeProject, loadOutputEvents, refreshOperationalViews, selectedOutputChannel]);

  const handleRegisterProject = useCallback(
    async (absolutePath: string, name?: string) => {
      setProjectBusy(true);
      setProjectError(null);
      try {
        const response = await registerProject(absolutePath, name);
        const projectsResponse = await getMyProjects();
        setProjects(projectsResponse.projects);
        await handleOpenProject(response.project.id, false);
      } catch (error) {
        setProjectError(error instanceof Error ? error.message : "Impossibile registrare il progetto.");
      } finally {
        setProjectBusy(false);
      }
    },
    [handleOpenProject],
  );

  const openFileInGroup = useCallback(
    async (path: string, line?: number, preferredGroupId?: string) => {
      if (!activeProject) return;

      // Punto unico (regola L): i documenti binari (.docx/.xlsx/.pdf...) non
      // sono leggibili come testo UTF-8. Aprirli qui chiamerebbe getProjectFile
      // -> /api/projects/:id/files -> read_to_string, che fallisce con HTTP 400
      // "Impossibile leggere il file come testo UTF-8". Sono i documenti
      // generati da nexus_doc_generate (link .docx nella chat) o file Office
      // aperti dall'albero. Non vanno nell'editor di codice: instradiamo al
      // pannello DOCUMENTI (Apri/Download/Rigenera/Elimina) e ne forziamo il
      // refresh, cosi' il documento appena generato compare subito.
      if (isBinaryDocPath(path)) {
        setActiveSidebarView("docs");
        setPrimarySidebarVisible(true);
        if (typeof window !== "undefined") {
          window.dispatchEvent(new CustomEvent("nexus:documents:refresh"));
        }
        return;
      }

      const existingGroup = editorGroups.find((group) =>
        group.tabs.some((tab) => tab.path === path),
      );
      if (existingGroup) {
        setActiveEditorGroupId(existingGroup.id);
        setEditorGroups((current) =>
          current.map((group) =>
            group.id === existingGroup.id ? { ...group, activePath: path } : group,
          ),
        );
        if (line) setPendingEditorFocus({ path, line });
        return;
      }

      try {
        const response = await getProjectFile(activeProject.id, path);
        const targetGroupId =
          preferredGroupId ??
          (layoutMode === "ai-center" ? "secondary" : "primary");
        setEditorGroups((current) =>
          current.map((group) => {
            if (group.id !== targetGroupId) return group;
            const nextTab = makeTab(response.path, response.content);
            return {
              ...group,
              tabs: [...group.tabs, nextTab],
              activePath: response.path,
            };
          }),
        );
        setActiveEditorGroupId(targetGroupId);
        if (line) setPendingEditorFocus({ path: response.path, line });
      } catch (error) {
        // Audit 27/05/2026: messaggio user-friendly per 404 file non trovato
        // (es. documento DB con file_path stantio dopo cancellazione filesystem).
        // Prima usciva il toast tecnico "API error 404: Not Found - Percorso non trovato"
        // che non aiutava l'utente a capire cosa fare.
        const rawMsg = error instanceof Error ? error.message : "Impossibile aprire il file.";
        const isNotFound =
          rawMsg.includes("API error 404") ||
          rawMsg.includes("Percorso non trovato") ||
          rawMsg.includes("non e' un file");
        if (isNotFound) {
          setProjectError(
            `File "${path}" non trovato sul filesystem. Il riferimento potrebbe essere stantio: aggiorna il pannello o rigenera il documento.`,
          );
        } else {
          setProjectError(rawMsg);
        }
      }
    },
    [activeProject, editorGroups, layoutMode],
  );
  // Bind ref dopo che openFileInGroup e' definito (bridge nexus:editor:open-file).
  openFileInGroupRef.current = openFileInGroup;

  const closeEditorTab = useCallback((groupId: string, path: string) => {
    setEditorGroups((current) =>
      current.map((group) => {
        if (group.id !== groupId) return group;
        const tabs = group.tabs.filter((tab) => tab.path !== path);
        const activePath =
          group.activePath === path ? tabs[tabs.length - 1]?.path ?? null : group.activePath;
        return {
          ...group,
          tabs,
          activePath,
        };
      }),
    );
  }, []);

  const confirmCloseDirtyTab = useCallback(
    async (tab: EditorTabState) => {
      if (!tab.dirty) return true;
      return confirmDialog(
        `Il file ${tab.title || tab.path} ha modifiche non salvate. Vuoi chiuderlo senza salvare?`,
        "Modifiche non salvate",
      );
    },
    [confirmDialog],
  );

  const saveActiveEditor = useCallback(async () => {
    if (!activeProject || !activeEditorTab) return;
    try {
      await saveProjectFile(activeProject.id, activeEditorTab.path, activeEditorTab.content ?? "");
      setEditorGroups((current) =>
        current.map((group) =>
          group.id === activeEditorGroupId
            ? {
                ...group,
                tabs: group.tabs.map((tab) =>
                  tab.path === activeEditorTab.path ? { ...tab, dirty: false } : tab,
                ),
              }
            : group,
        ),
      );
      const gitResponse = await getGitStatus(activeProject.id);
      setGitState(gitResponse.git);
      await refreshOperationalViews(activeProject.id);
      await loadOutputEvents(activeProject.id, selectedOutputChannel);
    } catch (error) {
      setProjectError(error instanceof Error ? error.message : "Impossibile salvare il file.");
    }
  }, [
    activeEditorGroupId,
    activeEditorTab,
    activeProject,
    loadOutputEvents,
    refreshOperationalViews,
    selectedOutputChannel,
  ]);

  const handleCreateEntry = useCallback(
    async (kind: "file" | "directory") => {
      if (!activeProject) return;
      const label = kind === "file" ? "nuovo file" : "nuova cartella";
      const path = await promptDialog(
        `Percorso relativo del ${label}:`,
        "",
        "Crea elemento",
      );
      if (!path?.trim()) return;
      try {
        await createProjectEntry(activeProject.id, path.trim(), kind);
        const opened = await openProject(activeProject.id);
        setTreeNodes(opened.tree);
        if (kind === "file") {
          await openFileInGroup(path.trim());
        }
      } catch (error) {
        setProjectError(error instanceof Error ? error.message : "Operazione file fallita.");
      }
    },
    [activeProject, openFileInGroup, promptDialog],
  );

  const handleRenameActive = useCallback(async () => {
    if (!activeProject || !activeEditorTab) return;
    const nextPath = await promptDialog(
      "Nuovo percorso relativo:",
      activeEditorTab.path,
      "Rinomina elemento",
    );
    if (!nextPath?.trim() || nextPath.trim() === activeEditorTab.path) return;
    try {
      await renameProjectEntry(activeProject.id, activeEditorTab.path, nextPath.trim());
      setEditorGroups((current) =>
        current.map((group) => ({
          ...group,
          activePath:
            group.activePath === activeEditorTab.path ? nextPath.trim() : group.activePath,
          tabs: group.tabs.map((tab) =>
            tab.path === activeEditorTab.path
              ? { ...tab, path: nextPath.trim(), title: basename(nextPath.trim()) }
              : tab,
          ),
        })),
      );
      const opened = await openProject(activeProject.id);
      setTreeNodes(opened.tree);
      setGitState(opened.git);
    } catch (error) {
      setProjectError(error instanceof Error ? error.message : "Impossibile rinominare l'elemento.");
    }
  }, [activeEditorTab, activeProject, promptDialog]);

  const handleDeleteActive = useCallback(async () => {
    if (!activeProject || !activeEditorTab) return;
    const confirmed = await confirmDialog(`Eliminare ${activeEditorTab.path}?`, "Conferma eliminazione");
    if (!confirmed) return;
    try {
      await deleteProjectEntry(activeProject.id, activeEditorTab.path);
      closeEditorTab(activeEditorGroupId, activeEditorTab.path);
      const opened = await openProject(activeProject.id);
      setTreeNodes(opened.tree);
      setGitState(opened.git);
      await refreshOperationalViews(activeProject.id);
    } catch (error) {
      setProjectError(error instanceof Error ? error.message : "Impossibile eliminare l'elemento.");
    }
  }, [activeEditorGroupId, activeEditorTab, activeProject, closeEditorTab, refreshOperationalViews, confirmDialog]);

  const handleSearch = useCallback(async () => {
    if (!activeProject || !searchQuery.trim()) return;
    setSearchBusy(true);
    try {
      const response = await searchProject(activeProject.id, searchQuery.trim());
      setSearchResults(response.results ?? []);
    } catch (error) {
      setProjectError(error instanceof Error ? error.message : "Ricerca non riuscita.");
    } finally {
      setSearchBusy(false);
    }
  }, [activeProject, searchQuery]);

  // ── Render helpers ────────────────────────────────────────────────────────

  const renderAiWorkspace = () => (
    <div
      style={{
        display: "grid",
        gridTemplateRows: "36px 1fr",
        minHeight: 0,
        minWidth: 0,
        width: "100%",
        height: "100%",
        flex: 1,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "3px 12px 3px 8px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgHeader,
          gap: 8,
        }}
      >
        <div style={{ color: tc.text, fontSize: 13, fontWeight: 700, whiteSpace: "nowrap" }}>AI Workspace</div>
        <ProfileSelector
          profiles={profilesMgr.profiles}
          selectedProfileId={selectedProfileId}
          onSelect={(id) => setSelectedProfileId(id)}
          onCreateNew={() => { setEditingProfile(undefined); setShowProfileEditor(true); }}
          style={{ flexShrink: 0 }}
        />
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            minWidth: 0,
            overflow: "hidden",
            flexShrink: 1,
          }}
          title="Comandi multi-chat"
        >
          <select
            value={multiChat.activeTabId ?? ""}
            onChange={(e) => {
              const next = e.target.value;
              if (!next) return;
              if (!multiChat.openTabs.includes(next)) {
                multiChat.openTab(next);
              } else {
                multiChat.setActiveTab(next);
              }
            }}
            title="Seleziona sessione chat"
            style={{
              borderRadius: 999,
              border: `1px solid ${tc.border}`,
              background: tc.bgInput,
              color: tc.textSecondary,
              padding: "2px 8px",
              fontSize: 11,
              fontFamily: "inherit",
              minWidth: 0,
              maxWidth: 210,
              flexShrink: 1,
            }}
          >
            {multiChat.allSessions.length === 0 ? (
              <option value="">Nessuna chat</option>
            ) : (
              multiChat.allSessions.map((session) => (
                <option key={session.id} value={session.id}>
                  {session.title}
                </option>
              ))
            )}
          </select>
          <button
            type="button"
            onClick={() => void multiChat.newSession()}
            title="Nuova chat"
            aria-label="Nuova chat"
            style={iconButton(tc)}
          >
            ＋
          </button>
          <button
            type="button"
            disabled={!multiChat.activeTabId}
            onClick={() => {
              const currentId = multiChat.activeTabId;
              if (!currentId) return;
              const currentTitle =
                multiChat.allSessions.find((session) => session.id === currentId)?.title ?? "";
              void (async () => {
                const next = await promptDialog("Nuovo nome chat", currentTitle, "Rinomina chat");
                if (next?.trim()) {
                  await multiChat.renameSession(currentId, next.trim());
                }
              })();
            }}
            title="Rinomina chat"
            aria-label="Rinomina chat"
            style={iconButton(tc, !multiChat.activeTabId)}
          >
            ✎
          </button>
          <button
            type="button"
            disabled={!multiChat.activeTabId}
            onClick={() => {
              const currentId = multiChat.activeTabId;
              if (!currentId) return;
              void (async () => {
                const ok = await confirmDialog("Eliminare questa chat? Tutti i messaggi saranno rimossi.");
                if (ok) {
                  await multiChat.deleteSession(currentId);
                }
              })();
            }}
            title="Elimina chat"
            aria-label="Elimina chat"
            style={iconButton(tc, !multiChat.activeTabId)}
          >
            🗑
          </button>
          {(() => {
            // % di riempimento context_window dell'ultimo turno della chat attiva.
            // Aggiornata da ChatPanel via onCtxRatioChange → multiChat.setCtxRatio.
            // Mostriamo il valore sul bottone "Compatta chat" cosi' l'utente vede
            // a colpo d'occhio quando e' opportuno compattare (>70% giallo, >=90% rosso).
            const activeId = multiChat.activeTabId;
            const ratio = activeId ? multiChat.ctxRatio.get(activeId) : undefined;
            const pct = ratio != null ? Math.round(ratio * 100) : null;
            const ratioColor = pct == null
              ? tc.textMuted
              : pct >= 90 ? tc.error
              : pct >= 70 ? tc.warning
              : tc.textMuted;
            return (
              <button
                type="button"
                disabled={!multiChat.activeTabId}
                onClick={() => {
                  const currentId = multiChat.activeTabId;
                  if (!currentId) return;
                  // compactSession aggiorna la barra token dalla risposta HTTP e
                  // mostra un toast (successo/errore): non dipende piu' dall'evento
                  // SSE ChatSessionCompacted, che puo' perdersi se subscribers=0.
                  // .catch: l'errore e' gia' notificato via toast dentro compactSession.
                  void multiChat.compactSession(currentId).catch(() => {});
                }}
                title={pct != null
                  ? `Compatta chat — context usato: ${pct}%`
                  : "Compatta chat"}
                aria-label={pct != null
                  ? `Compatta chat (context ${pct}%)`
                  : "Compatta chat"}
                style={{
                  ...iconButton(tc, !multiChat.activeTabId),
                  // Larghezza dinamica: il bottone si allarga per contenere
                  // l'icona + badge percentuale senza tagliare il testo,
                  // anche per valori a 4 cifre (es. 1952%).
                  width: "auto",
                  height: 30,
                  minWidth: 30,
                  maxWidth: "none",
                  flex: "0 0 auto",
                  display: "inline-flex",
                  alignItems: "center",
                  justifyContent: "center",
                  gap: 4,
                  paddingInline: pct != null ? 10 : 0,
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                }}
              >
                <span>⌁</span>
                {pct != null && (
                  <span
                    style={{
                      fontSize: 10,
                      fontWeight: 600,
                      color: ratioColor,
                      lineHeight: 1,
                    }}
                  >
                    {pct}%
                  </span>
                )}
              </button>
            );
          })()}
        </div>
      </div>
      <div
        style={{
          minHeight: 0,
          height: "100%",
          padding: 12,
          display: "flex",
          alignItems: "stretch",
          width: "100%",
          minWidth: 0,
          overflow: "hidden",
          boxSizing: "border-box",
        }}
      >
        {multiChat.activeTabId ? (
          <div style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", width: "100%" }}>
            <ChatPanel
              key={multiChat.activeTabId}
              projectId={activeProject?.id ?? "default"}
              activeFiles={allOpenPaths}
              sessionId={multiChat.activeTabId}
              profileId={selectedProfileId}
              onAgentActivityChange={handleChatAgentActivityChange}
              onCtxRatioChange={handleChatCtxRatioChange}
              selectedProvider={chatProvider}
              setSelectedProvider={(v) => { setChatProvider(v); try { localStorage.setItem("nexus:chatProvider", v); } catch {} }}
              selectedModel={chatModel}
              setSelectedModel={(v) => { setChatModel(v); try { localStorage.setItem("nexus:chatModel", v); } catch {} }}
              providerModels={chatProviderModels}
              automationMode={chatAutomationMode}
              setAutomationMode={(v) => { setChatAutomationMode(v); try { localStorage.setItem("nexus:chatAutomationMode", v); } catch {} }}
              supervisorMode={chatSupervisorMode}
              setSupervisorMode={(v) => { setChatSupervisorMode(v); try { localStorage.setItem("nexus:chatSupervisorMode", v); } catch {} }}
              showMemory={showMemory}
              setShowMemory={setShowMemory}
              externalInput={pendingChatMessage}
              externalAutoSend={pendingAutoSend}
              externalProviderHint={pendingProviderHint}
              externalAgentTypeHint={pendingAgentTypeHint}
              externalAutomationOverride={pendingExternalAutomation}
              onExternalInputConsumed={handleExternalInputConsumed}
              onTracesChange={setAiTraces}
              hasRunningServices={outputChannels.some((ch) => ch.label?.startsWith("●"))}
              onRunEnd={handleRunEnd}
            />
          </div>
        ) : null}
        {showProfileEditor && (
          <ProfileEditor
            profile={editingProfile}
            allProfiles={profilesMgr.profiles}
            onSave={async (payload) => {
              if (editingProfile) {
                await profilesMgr.update(editingProfile.id, payload);
              } else {
                const created = await profilesMgr.create(payload as import("../lib/api-client").CreateProfilePayload);
                setSelectedProfileId(created.id);
              }
            }}
            onDelete={editingProfile ? async () => {
              await profilesMgr.remove(editingProfile.id);
              setSelectedProfileId(DEFAULT_PROFILE_ID);
            } : undefined}
            onSetDefault={editingProfile ? async () => {
              await profilesMgr.setDefault(editingProfile.id);
            } : undefined}
            onClose={() => { setShowProfileEditor(false); setEditingProfile(undefined); }}
          />
        )}
      </div>
    </div>
  );

  const renderMainArea = () => {
    // Pannello editor condiviso ai tre layout. Se una nota KB e' aperta
    // (openNoteId, emesso da notes-tab/code-wiki-tab via nexus:note:open) mostra
    // NoteDetail al posto dell'editor. Prima questa logica esisteva SOLO nel
    // layout split-ai-editor: nei layout ai-center / editor-center la nota non
    // si apriva (bug 4e27af1) — l'evento partiva ma nessun pannello lo guardava.
    const editorAreaEl = (
      <EditorArea
        editorGroups={editorGroups}
        activeEditorGroupId={activeEditorGroupId}
        activeProject={activeProject}
        problemItems={problemItems}
        onSetActiveGroup={setActiveEditorGroupId}
        onSetEditorGroups={setEditorGroups}
        onSaveActive={() => void saveActiveEditor()}
        onRenameActive={() => void handleRenameActive()}
        onDeleteActive={() => void handleDeleteActive()}
        onConfirmCloseTab={confirmCloseDirtyTab}
      />
    );
    const editorPanel = openNoteId ? (
      <div style={{ height: "100%", overflow: "auto", background: tc.bg }}>
        <NoteDetail
          projectId={activeProject?.id ?? ""}
          noteId={openNoteId}
          onBack={() => setOpenNoteId(null)}
        />
      </div>
    ) : (
      editorAreaEl
    );
    if (layoutMode === "ai-center") {
      return (
        <div
          style={{
            minHeight: 0,
            height: "100%",
            display: "grid",
            gridTemplateColumns: `minmax(0, 1fr) ${effectiveRightWidth}px`,
          }}
        >
          <div style={{ minWidth: 0, minHeight: 0, height: "100%", display: "flex", overflow: "hidden", borderRight: `1px solid ${tc.border}`, position: "relative" }}>
            {renderAiWorkspace()}
            <div
              {...resizeCenter}
              style={{
                position: "absolute",
                top: 0,
                right: -3,
                bottom: 0,
                width: 6,
                cursor: "col-resize",
                background: "transparent",
                zIndex: 10,
                transition: "background 0.15s",
              }}
              onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.accent + "44"; }}
              onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
            />
          </div>
          <div
            style={{
              minWidth: 0,
              minHeight: 0,
              height: "100%",
              overflow: "hidden",
              display: "grid",
              gridTemplateRows: "26px minmax(0, 1fr)",
            }}
          >
            <RightViewTabs
              rightView={rightView}
              setRightView={setRightView}
              tc={tc}
            />
            {rightView === "editor" ? (
              editorPanel
            ) : (
              <SqlQueryPanel project={activeProject} />
            )}
          </div>
        </div>
      );
    }

    if (layoutMode === "editor-center") {
      return (
        <div style={{ minHeight: 0, height: "100%" }}>
          {editorPanel}
        </div>
      );
    }

    // split-ai-editor
    return (
      <div
        style={{
          minHeight: 0,
          height: "100%",
          display: "grid",
          gridTemplateColumns: `minmax(0, 1fr) ${effectiveRightWidth}px`,
        }}
      >
        <div style={{ minWidth: 0, minHeight: 0, height: "100%", display: "flex", overflow: "hidden", borderRight: `1px solid ${tc.border}`, position: "relative" }}>
          {renderAiWorkspace()}
          {/* Resize handle sovrapposto al bordo destro del pannello AI */}
          <div
            {...resizeCenter}
            style={{
              position: "absolute",
              top: 0,
              right: -3,
              bottom: 0,
              width: 6,
              cursor: "col-resize",
              background: "transparent",
              zIndex: 10,
              transition: "background 0.15s",
            }}
            onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.accent + "44"; }}
            onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
          />
        </div>
        <div style={{ minWidth: 0, minHeight: 0, height: "100%", overflow: "hidden" }}>
          {editorPanel}
        </div>
      </div>
    );
  };

  return (
    <main
      style={{
        height: "100vh",
        display: "grid",
        gridTemplateColumns: `${activityBarWidth}px ${primarySidebarVisible ? `${effectiveLeftWidth}px` : "0px"} minmax(0, 1fr) ${showSecondaryAi ? `${effectiveRightWidth}px` : "0px"}`,
        gridTemplateRows: `48px minmax(0, 1fr) ${bottomPanelVisible ? `${bottomHeight}px` : "0px"} 24px`,
        background: tc.bg,
        overflow: "hidden",
        boxSizing: "border-box",
      }}
    >
      {/* ── Top header ─────────────────────────────────────────────────────── */}
      <TopBar
        tc={tc}
        isMobileViewport={isMobileViewport}
        isNarrowViewport={isNarrowViewport}
        activeProject={activeProject}
        projects={projects}
        layoutMode={layoutMode}
        primarySidebarVisible={primarySidebarVisible}
        bottomPanelVisible={bottomPanelVisible}
        isFullscreen={isFullscreen}
        providerStatus={providerStatus}
        onTogglePrimarySidebar={() => setPrimarySidebarVisible((current) => !current)}
        onToggleBottomPanel={() => setBottomPanelVisible((current) => !current)}
        onCycleLayoutMode={cycleLayoutMode}
        onToggleFullscreen={() => { void toggleFullscreen(); }}
        onSelectProject={async (projectId) => {
          await handleOpenProject(projectId);
          window.history.replaceState(null, "", "/?project=" + projectId);
        }}
        onRegisterProject={handleRegisterProject}
        onRefreshProjects={async () => {
          try {
            const response = await getMyProjects();
            setProjects(response.projects);
          } catch { /* ignore */ }
        }}
      />

      {/* ── Overlay prima analisi: copre il workbench finche' l'analisi non e' completata ── */}
      {activeProject && !activeProject.isAnalyzed && (
        <FirstAnalysisOverlay
          tc={tc}
          analysisInProgress={analysisInProgress}
          analysisStep={analysisStep}
          onAnalyze={() => { if (activeProject) void runFirstAnalysis(activeProject.id); }}
          onSkip={() => {
            // Permetti di entrare comunque senza analisi (power user)
            setActiveProject(prev => prev ? { ...prev, isAnalyzed: true, nexusReady: true } : prev);
          }}
        />
      )}

      {/* ── Activity bar (icon column) ─────────────────────────────────────── */}
      <ActivityBar
        tc={tc}
        activityButtonSize={activityButtonSize}
        activeSidebarView={activeSidebarView}
        onSelectView={(view) => {
          if (activeSidebarView === view && primarySidebarVisible) {
            // Clic sulla voce già attiva → chiude il pannello (toggle)
            setPrimarySidebarVisible(false);
          } else {
            setActiveSidebarView(view);
            setPrimarySidebarVisible(true);
          }
        }}
      />

      {/* ── Primary sidebar ───────────────────────────────────────────────── */}
      <section
        style={{
          gridRow: "2",
          gridColumn: "2",
          minWidth: 0,
          overflow: "hidden",
          borderRight: primarySidebarVisible ? `1px solid ${tc.border}` : "none",
          background: tc.bgSidebar,
          display: primarySidebarVisible ? "flex" : "none",
          flexDirection: "column",
          position: "relative",
          zIndex: 1,
        }}
      >
        <SidebarManager
          activeSidebarView={activeSidebarView}
          project={activeProject}
          treeNodes={treeNodes}
          git={gitState}
          activeEditorTab={activeEditorTab}
          allOpenTabs={allOpenTabs}
          currentBranch={currentBranch}
          runConfigs={runConfigs}
          onRunConfigsChange={setRunConfigs}
          onLaunchConfig={() => {
            setActivePanelTab("services");
          }}
          searchQuery={searchQuery}
          searchBusy={searchBusy}
          searchResults={searchResults}
          onSetSearchQuery={setSearchQuery}
          onSearch={() => void handleSearch()}
          onOpenFile={(path, line, groupId) => void openFileInGroup(path, line, groupId)}
          onSaveActive={() => void saveActiveEditor()}
          onCreateEntry={(kind) => void handleCreateEntry(kind)}
          onRefreshProject={() => void refreshProject()}
          onProjectAnalyzed={() => void profilesMgr.reload()}
          onSendToChat={(msg, opts) => {
            setPendingChatMessage(msg);
            setPendingAutoSend(true);
            if (opts?.providerHint || opts?.modelHint) {
              setPendingProviderHint({ provider: opts.providerHint, model: opts.modelHint });
            }
          }}
          onFileTreeChanged={() => void refreshProject()}
          onFileDeleted={(path) => {
            // Chiudi eventuali tab editor aperti su questo path in tutti i gruppi.
            setEditorGroups((groups) =>
              groups.map((g) => ({
                ...g,
                tabs: g.tabs.filter((t) => t.path !== path),
                activePath: g.activePath === path
                  ? (g.tabs.filter((t) => t.path !== path).at(-1)?.path ?? null)
                  : g.activePath,
              })),
            );
          }}
          onFileRenamed={(oldPath, newPath) => {
            // Aggiorna i tab editor che puntavano al vecchio path.
            setEditorGroups((groups) =>
              groups.map((g) => ({
                ...g,
                tabs: g.tabs.map((t) =>
                  t.path === oldPath
                    ? { ...t, path: newPath, title: newPath.split("/").pop() ?? newPath }
                    : t,
                ),
                activePath: g.activePath === oldPath ? newPath : g.activePath,
              })),
            );
          }}
        />
      </section>

      {/* Left resize handle */}
      {primarySidebarVisible && (
        <div
          {...resizeLeft}
          style={{
            gridRow: "2",
            gridColumn: "2",
            justifySelf: "end",
            alignSelf: "stretch",
            width: 6,
            cursor: "col-resize",
            zIndex: 20,
            position: "relative",
            background: "transparent",
            transition: "background 0.15s",
          }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.accent + "55"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
        />
      )}

      {/* ── Main editor area ──────────────────────────────────────────────── */}
      <section
        style={{
          gridRow: "2",
          gridColumn: "3",
          minWidth: 0,
          minHeight: 0,
          width: "100%",
          height: "100%",
          overflow: "hidden",
          background: tc.bg,
          position: "relative",
          isolation: "isolate",
        }}
      >
        {renderMainArea()}
      </section>

      {/* ── Secondary sidebar (AI) ────────────────────────────────────────── */}
      {showSecondaryAi && (
        <>
          {/* Right resize handle — stesso pattern del lato sinistro */}
          <div
            {...resizeRight}
            style={{
              gridRow: "2",
              gridColumn: "3",
              justifySelf: "end",
              alignSelf: "stretch",
              width: 6,
              cursor: "col-resize",
              zIndex: 20,
              position: "relative",
              background: "transparent",
              transition: "background 0.15s",
            }}
            onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.accent + "55"; }}
            onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
          />
          <div
            style={{
              gridRow: "2",
              gridColumn: "4",
              minWidth: 0,
              minHeight: 0,
              overflow: "hidden",
              borderLeft: `1px solid ${tc.border}`,
              background: tc.bgSidebar,
              display: "flex",
              flexDirection: "column",
            }}
          >
            {renderAiWorkspace()}
          </div>
        </>
      )}

      {/* ── Bottom panel resize handle ────────────────────────────────────── */}
      {bottomPanelVisible && (
        <div
          {...resizeBottom}
          style={{
            gridColumn: "2 / 5",
            gridRow: "3",
            alignSelf: "start",
            height: 6,
            cursor: "row-resize",
            zIndex: 20,
            position: "relative",
            background: "transparent",
            transition: "background 0.15s",
          }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLDivElement).style.background = tc.accent + "55"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
        />
      )}

      {/* ── Bottom panel ──────────────────────────────────────────────────── */}
      <section
        style={{
          gridRow: "3",
          gridColumn: "2 / 5",
          minHeight: 0,
          overflow: "hidden",
          borderTop: bottomPanelVisible ? `1px solid ${tc.border}` : "none",
          background: tc.bgSidebar,
          display: bottomPanelVisible ? "grid" : "none",
          gridTemplateRows: "34px 1fr",
        }}
      >
        <BottomPanelHeader
          tc={tc}
          isMobileViewport={isMobileViewport}
          activePanelTab={activePanelTab}
          activeProject={activeProject}
          onSelectTab={(tab) => setActivePanelTab(tab)}
          onHide={() => setBottomPanelVisible(false)}
        />
        <div style={{ minHeight: 0, overflow: "hidden" }}>
          <BottomPanelManager
            activePanelTab={activePanelTab}
            onSelectPanelTab={(tab) => setActivePanelTab(tab)}
            project={activeProject}
            problemItems={problemItems}
            outputChannels={outputChannels}
            selectedOutputChannel={selectedOutputChannel}
            outputEvents={outputEvents}
            ports={portsFromDispatcher.length > 0
              ? portsFromDispatcher.map((p) => ({ port: p.port, label: p.label, state: "listen" }))
              : ports}
            playwrightRuns={playwrightRunsFromDispatcher.length > 0 ? playwrightRunsFromDispatcher : playwrightRuns}
            playwrightConfigured={playwrightConfigured}
            onOpenFile={(path, line) => void openFileInGroup(path, line)}
            onSelectOutputChannel={setSelectedOutputChannel}
            onRefreshPanel={(tab) => {
              if (!activeProject) return;
              if (tab === "services" || tab === "output") {
                void loadOutputEvents(activeProject.id, selectedOutputChannel);
                return;
              }
              void refreshOperationalViews(activeProject.id);
            }}
            onClearPanel={(tab) => {
              switch (tab) {
                case "problems": setProblemItems([]); break;
                case "services":
                case "output":
                  setOutputEvents([]);
                  // Salva il timestamp di clear per questo canale: gli eventi precedenti
                  // verranno esclusi anche dopo remount/cambio-tab (persistito in sessionStorage).
                  clearTimestamps.current[selectedOutputChannel] = Date.now();
                  try {
                    if (typeof sessionStorage !== "undefined") {
                      sessionStorage.setItem(
                        "nexus:clearTimestamps",
                        JSON.stringify(clearTimestamps.current),
                      );
                    }
                  } catch { /* ignora */ }
                  break;  // output = alias legacy
                case "ports": setPorts([]); break;
                case "playwright":
                  setPlaywrightRuns([]);
                  if (activeProject) {
                    void clearPlaywrightRuns(activeProject.id).catch((err) => {
                      console.warn("[playwright] clear runs failed:", err);
                    });
                  }
                  break;
              }
            }}
            onSendToChat={(msg) => {
              setPendingChatMessage(msg);
              setPendingAutoSend(true);
              setPendingExternalAutomation("confirm");
              // Pulsanti del bottom panel = workflow d'azione (error-fix): forza
              // l'hint cosi' il backend salta la disambiguazione d'intent A/B.
              setPendingAgentTypeHint(ACTION_AGENT_HINT);
            }}
            onAutoSendToChat={(msg) => {
              setPendingChatMessage(msg);
              setPendingAutoSend(true);
              setPendingExternalAutomation("confirm");
              setPendingAgentTypeHint(ACTION_AGENT_HINT);
            }}
            onKillPort={async (port) => {
              if (!activeProject) return;
              // Optimistic update: rimuovi subito la porta dalla lista, cosi'
              // l'utente vede feedback immediato senza dover aspettare il
              // refresh (1-2s). Se la porta torna a essere live nel refresh
              // successivo, riapparira' da sola — segno che il kill non e'
              // riuscito (es. docker-proxy respawned, processo non killabile).
              setPorts((current) => current.filter((p) => p.port !== port));
              try {
                const res = await killPortProcess(activeProject.id, port);
                // Attesa breve: il kernel impiega ~100-300ms a rilasciare il
                // socket. Senza questa attesa il refresh successivo a volte
                // ritrova la porta in TIME_WAIT e la rimostra.
                await new Promise((r) => setTimeout(r, 800));
                await refreshOperationalViews(activeProject.id);
                if (!res.freed) {
                  console.warn(
                    `[ports] kill port ${port}: backend ha eseguito kill ma la porta non e' stata liberata. deleted_allocations=${res.deleted_allocations}`,
                  );
                  void alertDialog(
                    `Il backend ha provato a terminare il processo ma la porta ${port} risulta ancora in ascolto. ` +
                    `Probabile causa: container Docker (il proxy si rigenera automaticamente) o processo non killabile. ` +
                    `Allocazione DB rimossa: ${res.deleted_allocations}.`,
                    "Porta non liberata",
                  );
                }
              } catch (e) {
                console.error("[ports] killPortProcess fallito:", e);
                // Ripristina la porta nella UI se il kill ha fallito.
                await refreshOperationalViews(activeProject.id);
              }
            }}
            agentRunEndSignal={agentRunEndSignal}
            traces={aiTraces}
            onClearTraces={() => setAiTraces([])}
          />
        </div>
      </section>

      {/* ── Status bar ────────────────────────────────────────────────────── */}
      <StatusBar
        tc={tc}
        currentBranch={currentBranch}
        projectName={activeProject?.name ?? "nessun progetto"}
        layoutMode={layoutMode}
        problemCount={problemCount}
        liveHealth={liveHealth}
      />

      {/* ── Overlays ──────────────────────────────────────────────────────── */}
      <ShellOverlays
        tc={tc}
        projectBusy={projectBusy}
        projectError={projectError}
        liveHealth={liveHealth}
      />
    </main>
  );
}
