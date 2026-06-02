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
import { ProjectSwitcher } from "./project-switcher";
import { UserSidebarMenu } from "./user-header";
import { QuotaBadge } from "./panels/quota-badge";
import { NoteDetail } from "./knowledge/note-detail";
import type { SidebarView } from "./sidebar/sidebar-manager";
import type { PanelTab } from "./panels/bottom-panel-manager";
import { TruncatedText } from "./truncated-text";
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
import { ConnectionStatusBadge, ToastStack, usePanelHighlight } from "./dispatcher-status";

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

type SecondarySidebarView = "ai-tools";
type ProviderKey = "openai" | "anthropic" | "google" | "deepseek" | "mistral";
type ProviderHealthState = {
  ok: boolean | null;
  reason?: string;
  status?: string;
  billing?: boolean; // true = crediti/quota esauriti → pallino giallo
};

const sidebarItems: Array<{ key: SidebarView; label: string; icon: string }> = [
  { key: "project-db", label: "Database", icon: "🗄" },
  { key: "knowledge", label: "Knowledge", icon: "🧠" },
  { key: "explorer", label: "Explorer", icon: "🗂" },
  { key: "search", label: "Ricerca", icon: "🔍" },
  { key: "source-control", label: "Git", icon: "⑂" },
  { key: "run", label: "Run", icon: "▶" },
  { key: "docs", label: "Documenti", icon: "📄" },
  { key: "server-monitor", label: "Monitor", icon: "▣" },
];

const panelTabs: Array<{ key: PanelTab; label: string }> = [
  { key: "problems", label: "Problemi" },
  { key: "terminal", label: "Terminale" },
  { key: "run", label: "Run & Debug" },
  { key: "debug", label: "Console Debug" },
  { key: "ports", label: "Porte" },
  { key: "services", label: "Servizi" },
  { key: "playwright", label: "Playwright" },
  { key: "monitor", label: "Monitor" },
  { key: "optimization", label: "Ottimizzazione" },
  { key: "security", label: "Sicurezza" },
];

const EMPTY_GROUPS: EditorGroupState[] = [
  { id: "primary", tabs: [], activePath: null },
  { id: "secondary", tabs: [], activePath: null },
];

function basename(path: string) {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function makeTab(path: string, content: string, dirty = false): EditorTabState {
  return {
    path,
    title: basename(path),
    dirty,
    pinned: true,
    content,
  };
}

function defaultWorkbenchState(): WorkbenchState {
  return {
    layoutMode: "ai-center",
    primarySidebarVisible: true,
    secondarySidebarVisible: false,
    secondarySidebarView: "ai-tools",
    layoutControlStyle: "icon-menu",
    iconButtonsOnly: true,
    bottomPanelVisible: true,
    activeSidebarView: "explorer",
    activePanelTab: "terminal",
    leftWidth: 300,
    rightWidth: 430,
    bottomHeight: 250,
    editorGroups: EMPTY_GROUPS,
    ai: {
      activeContextPaths: [],
    },
    terminal: {
      activeTabId: "shell-1",
      tabs: [{ id: "shell-1", title: "shell 1" }],
    },
  };
}

function normalizeWorkbenchState(input?: Partial<WorkbenchState> | null): WorkbenchState {
  const defaults = defaultWorkbenchState();
  const groups = (input?.editorGroups ?? EMPTY_GROUPS)
    .slice(0, 2)
    .map((group, index) => ({
      id: group.id || (index === 0 ? "primary" : "secondary"),
      activePath: group.activePath ?? null,
      tabs: (group.tabs ?? []).map((tab) => ({
        path: tab.path,
        title: tab.title || basename(tab.path),
        dirty: Boolean(tab.dirty),
        pinned: tab.pinned !== false,
        content: tab.content ?? "",
      })),
    }));

  while (groups.length < 2) {
    groups.push({ id: groups.length === 0 ? "primary" : "secondary", tabs: [], activePath: null });
  }

  return {
    ...defaults,
    ...input,
    layoutMode: (input?.layoutMode as WorkbenchLayoutMode | undefined) ?? defaults.layoutMode,
    secondarySidebarVisible:
      typeof input?.secondarySidebarVisible === "boolean"
        ? input.secondarySidebarVisible
        : defaults.secondarySidebarVisible,
    secondarySidebarView:
      (input?.secondarySidebarView as SecondarySidebarView | undefined) ??
      defaults.secondarySidebarView,
    layoutControlStyle:
      input?.layoutControlStyle === "icon-menu" ? "icon-menu" : defaults.layoutControlStyle,
    iconButtonsOnly:
      typeof input?.iconButtonsOnly === "boolean"
        ? input.iconButtonsOnly
        : defaults.iconButtonsOnly,
    activeSidebarView:
      (input?.activeSidebarView as SidebarView | undefined) ?? defaults.activeSidebarView,
    activePanelTab: (panelTabs.some((t) => t.key === input?.activePanelTab)
      ? (input!.activePanelTab as PanelTab)
      // "output" era un alias di "services" — reindirizza
      : input?.activePanelTab === "output" ? "services"
      : defaults.activePanelTab),
    editorGroups: groups,
    ai: {
      ...defaults.ai,
      ...(input?.ai ?? {}),
    },
    terminal: {
      ...defaults.terminal,
      ...(input?.terminal ?? {}),
      tabs: input?.terminal?.tabs?.length ? input.terminal.tabs : defaults.terminal.tabs,
    },
  };
}

async function hydrateGroups(
  projectId: string,
  state: WorkbenchState,
  fallbackPaths: string[],
): Promise<EditorGroupState[]> {
  const tabsByPath = new Map<string, EditorTabState>();

  for (const group of state.editorGroups) {
    for (const tab of group.tabs) {
      tabsByPath.set(tab.path, {
        ...tab,
        title: tab.title || basename(tab.path),
        content: tab.content ?? "",
      });
    }
  }

  for (const path of fallbackPaths) {
    if (!tabsByPath.has(path)) {
      tabsByPath.set(path, makeTab(path, ""));
    }
  }

  await Promise.all(
    [...tabsByPath.values()].map(async (tab) => {
      if (typeof tab.content === "string" && tab.content.length > 0) {
        return;
      }
      try {
        const response = await getProjectFile(projectId, tab.path);
        tab.content = response.content;
        tab.title = basename(response.path);
      } catch {
        tab.content = "";
      }
    }),
  );

  const assigned = new Set<string>();
  const groups = state.editorGroups.map((group) => {
    const tabs = group.tabs
      .map((tab) => tabsByPath.get(tab.path))
      .filter((tab): tab is EditorTabState => Boolean(tab))
      .map((tab) => {
        assigned.add(tab.path);
        return tab;
      });
    const activePath =
      group.activePath && tabs.some((tab) => tab.path === group.activePath)
        ? group.activePath
        : tabs[0]?.path ?? null;
    return {
      id: group.id,
      tabs,
      activePath,
    };
  });

  const leftovers = [...tabsByPath.values()].filter((tab) => !assigned.has(tab.path));
  if (leftovers.length > 0) {
    groups[0].tabs = [...groups[0].tabs, ...leftovers];
    if (!groups[0].activePath) {
      groups[0].activePath = leftovers[0].path;
    }
  }

  return groups;
}

function StatusDot({ ok, billing }: { ok: boolean | null; billing?: boolean }) {
  const color =
    ok === null ? "#94a3b8"
    : ok ? "#4ade80"
    : billing ? "#facc15"   // giallo per crediti/quota esauriti
    : "#f87171";             // rosso per errori reali
  return (
    <span
      style={{
        display: "inline-block",
        width: 8,
        height: 8,
        borderRadius: "50%",
        background: color,
      }}
    />
  );
}

function summarizeProviderReason(reason?: string): string | undefined {
  if (!reason) return undefined;
  const normalized = reason.replace(/\r\n/g, "\n").trim();
  if (!normalized) return undefined;
  const cutTokens = [
    "\n[",
    "\nlinks",
    "\nviolations",
    "\n* ",
    " To monitor your current usage",
    " For more information on this error",
    " Please retry in",
  ];
  let shortened = normalized;
  for (const token of cutTokens) {
    const idx = shortened.indexOf(token);
    if (idx > 0) {
      shortened = shortened.slice(0, idx).trim();
    }
  }
  const firstLine = shortened.split("\n")[0]?.trim() ?? shortened;
  return firstLine.length > 220 ? `${firstLine.slice(0, 217)}...` : firstLine;
}

function providerTitle(label: string, state: ProviderHealthState): string {
  if (state.ok === null) {
    return `${label} stato sconosciuto`;
  }
  if (state.ok) {
    return state.status ? `${label} disponibile (${state.status})` : `${label} disponibile`;
  }
  const message = summarizeProviderReason(state.reason);
  if (message) {
    return `${label} errore: ${message}`;
  }
  return `${label} non disponibile`;
}

function iconButton(tc: ReturnType<typeof useThemeColors>, disabled = false, active = false) {
  return {
    width: 30,
    height: 30,
    border: `1px solid ${active ? tc.accent : tc.border}`,
    background: disabled ? tc.bgInput : active ? tc.accentBg : tc.bgCard,
    color: disabled ? tc.textMuted : active ? tc.accent : tc.textSecondary,
    borderRadius: 7,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    lineHeight: 1,
  } as const;
}

export function IdeShell({ dashboard, initialProjectId }: { dashboard: DashboardSnapshot; initialProjectId?: string }) {
  const tc = useThemeColors();
  const { promptDialog, confirmDialog, alertDialog } = useGlobalDialog();
  // Polling client-side dello health: il prop `dashboard` è uno snapshot SSR
  // e non si aggiorna mai. Senza questo, i LED DB/Redis/Brain restano congelati
  // sullo stato del primo render della pagina /ide.
  const [liveHealth, setLiveHealth] = useState<{ database: boolean; redis: boolean; neural_core: boolean; tools_grpc?: boolean; brain_rest?: boolean }>(
    dashboard.health ?? { database: false, redis: false, neural_core: false, tools_grpc: false, brain_rest: false }
  );
  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const h = await getHealth();
        if (!cancelled) setLiveHealth(h.components);
      } catch {
        if (!cancelled) setLiveHealth({ database: false, redis: false, neural_core: false, tools_grpc: false, brain_rest: false });
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

  // Bridge `nexus:kb:open-code-doc` -> apre la sidebar Knowledge (la tab
  // Code Wiki e la selezione della nota del file sono gestite da KnowledgePanel
  // e CodeWikiTab, che ascoltano lo stesso evento). Navigazione codice -> doc.
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
      const hydratedGroups = await hydrateGroups(project.id, normalized, sessionActivePaths);
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
                  // Il backend emette ChatSessionCompacted via dispatcher SSE,
                  // use-chat ascolta e riallinea tokenUsage senza re-mount.
                  void multiChat.compactSession(currentId);
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
          {openNoteId ? (
            <div style={{ height: "100%", overflow: "auto", background: tc.bg }}>
              <NoteDetail
                projectId={activeProject?.id ?? ""}
                noteId={openNoteId}
                onBack={() => setOpenNoteId(null)}
              />
            </div>
          ) : (
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
          )}
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
      <header
        style={{
          gridColumn: "1 / 5",
          display: "flex",
          alignItems: "center",
          columnGap: 10,
          padding: "0 12px",
          background: tc.bgHeader,
          borderBottom: `1px solid ${tc.border}`,
          flexWrap: isMobileViewport ? "wrap" : "nowrap",
          rowGap: isMobileViewport ? 6 : 0,
        }}
      >
        <a
          href="/?site"
          title="Vedi sito"
          style={{
            fontSize: 13,
            letterSpacing: "0.08em",
            color: tc.text,
            fontWeight: 700,
            textDecoration: "none",
            cursor: "pointer",
          }}
        >
          NEXUS
        </a>
        <TruncatedText
          text={activeProject?.name ?? "Nessun progetto"}
          maxWidth={220}
          tc={tc}
          style={{ color: tc.textMuted, fontSize: 12 }}
        />
        <div style={{ width: 1, height: 20, background: tc.border }} />
        <button
          type="button"
          onClick={() => setPrimarySidebarVisible((current) => !current)}
          title={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
          aria-label={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
          style={iconButton(tc, false, primarySidebarVisible)}
        >
          ◧
        </button>
        <button
          type="button"
          onClick={() => setBottomPanelVisible((current) => !current)}
          title={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
          aria-label={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
          style={iconButton(tc, false, bottomPanelVisible)}
        >
          <span style={{ display: "inline-block", transform: "rotate(90deg)" }}>◧</span>
        </button>
        <button
          type="button"
          onClick={cycleLayoutMode}
          title={`Cambia layout (${layoutMode})`}
          aria-label={`Cambia layout (${layoutMode})`}
          style={iconButton(tc)}
        >
          ⧉
        </button>
        <button
          type="button"
          onClick={() => {
            void toggleFullscreen();
          }}
          title={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
          aria-label={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
          style={iconButton(tc, false, isFullscreen)}
        >
          {isFullscreen ? "🗗" : "🗖"}
        </button>
        <div style={{ flex: 1, minWidth: 0, order: isMobileViewport ? 10 : 0 }}>
          <ProjectSwitcher
            projects={projects}
            activeProjectId={activeProject?.id}
            compact={isMobileViewport}
            onSelect={async (projectId) => {
              await handleOpenProject(projectId);
              window.history.replaceState(null, "", "/?project=" + projectId);
            }}
            onRegister={handleRegisterProject}
            onRefreshProjects={async () => {
              try {
                const response = await getMyProjects();
                setProjects(response.projects);
              } catch { /* ignore */ }
            }}
          />
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            columnGap: isNarrowViewport ? 8 : 10,
            marginLeft: 8,
            flexShrink: 0,
            maxWidth: isNarrowViewport ? 320 : undefined,
            overflowX: isNarrowViewport ? "auto" : "visible",
            paddingBottom: isNarrowViewport ? 2 : 0,
            order: isMobileViewport ? 11 : 0,
            flexWrap: isMobileViewport ? "wrap" : "nowrap",
            rowGap: isMobileViewport ? 6 : 0,
            whiteSpace: "nowrap",
          }}
          aria-label="Stato provider AI"
        >
          <ConnectionStatusBadge />
          <span
            title={providerTitle("OpenAI", providerStatus.openai)}
            style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
          >
            <StatusDot ok={providerStatus.openai.ok} billing={providerStatus.openai.billing} />
            {!isNarrowViewport && "OpenAI"}
          </span>
          <span
            title={providerTitle("Anthropic", providerStatus.anthropic)}
            style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
          >
            <StatusDot ok={providerStatus.anthropic.ok} billing={providerStatus.anthropic.billing} />
            {!isNarrowViewport && "Anthropic"}
          </span>
          <span
            title={providerTitle("Google", providerStatus.google)}
            style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
          >
            <StatusDot ok={providerStatus.google.ok} billing={providerStatus.google.billing} />
            {!isNarrowViewport && "Google"}
          </span>
          <span
            title={providerTitle("DeepSeek", providerStatus.deepseek)}
            style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
          >
            <StatusDot ok={providerStatus.deepseek.ok} billing={providerStatus.deepseek.billing} />
            {!isNarrowViewport && "DeepSeek"}
          </span>
          <span
            title={providerTitle("Mistral", providerStatus.mistral)}
            style={{ display: "inline-flex", alignItems: "center", gap: 4, color: tc.textMuted, fontSize: 11 }}
          >
            <StatusDot ok={providerStatus.mistral.ok} billing={providerStatus.mistral.billing} />
            {!isNarrowViewport && "Mistral"}
          </span>
        </div>
      </header>

      {/* ── Overlay prima analisi: copre il workbench finche' l'analisi non e' completata ── */}
      {activeProject && !activeProject.isAnalyzed && (
        <div
          style={{
            gridRow: "2 / 5",
            gridColumn: "1 / 5",
            zIndex: 100,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            background: tc.bg,
          }}
        >
          <div style={{
            maxWidth: 520,
            textAlign: "center",
            padding: 40,
            borderRadius: 12,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            boxShadow: "0 8px 32px rgba(0,0,0,0.18)",
          }}>
            <div style={{ fontSize: 36, marginBottom: 16 }}>
              {analysisInProgress ? "⚙️" : "📂"}
            </div>
            <div style={{ fontSize: 16, fontWeight: 700, color: tc.text, marginBottom: 8 }}>
              {analysisInProgress ? "Analisi in corso..." : "Progetto non analizzato"}
            </div>
            <div style={{ fontSize: 13, color: tc.textSecondary, marginBottom: 20, lineHeight: "1.5" }}>
              {analysisInProgress
                ? analysisStep || "Nexus sta analizzando la struttura del progetto, i linguaggi, i framework e le configurazioni di esecuzione."
                : "Nexus deve analizzare il progetto prima di poter offrire le funzionalita' complete (servizi, comandi, diagnostica, AI contestuale)."}
            </div>
            {analysisInProgress ? (
              <div style={{
                height: 4,
                borderRadius: 2,
                background: tc.border,
                overflow: "hidden",
                marginBottom: 12,
              }}>
                <div style={{
                  height: "100%",
                  background: tc.accent,
                  borderRadius: 2,
                  animation: "nexus-analysis-progress 2s ease-in-out infinite",
                  width: "40%",
                }} />
                <style>{`
                  @keyframes nexus-analysis-progress {
                    0% { transform: translateX(-100%); }
                    100% { transform: translateX(350%); }
                  }
                `}</style>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => { if (activeProject) void runFirstAnalysis(activeProject.id); }}
                style={{
                  background: tc.accent,
                  color: "#fff",
                  border: "none",
                  borderRadius: 8,
                  padding: "10px 28px",
                  fontSize: 14,
                  fontWeight: 600,
                  cursor: "pointer",
                  fontFamily: "inherit",
                }}
              >
                Analizza progetto
              </button>
            )}
            {!analysisInProgress && (
              <div style={{ marginTop: 12 }}>
                <button
                  type="button"
                  onClick={() => {
                    // Permetti di entrare comunque senza analisi (power user)
                    setActiveProject(prev => prev ? { ...prev, isAnalyzed: true, nexusReady: true } : prev);
                  }}
                  style={{
                    background: "transparent",
                    color: tc.textMuted,
                    border: "none",
                    fontSize: 11,
                    cursor: "pointer",
                    textDecoration: "underline",
                    fontFamily: "inherit",
                  }}
                >
                  Salta e continua senza analisi
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {/* ── Activity bar (icon column) ─────────────────────────────────────── */}
      <aside
        style={{
          gridRow: "2 / 4",
          gridColumn: "1",
          borderRight: `1px solid ${tc.border}`,
          background: tc.bgSidebar,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 8,
          padding: "10px 6px",
        }}
      >
        {sidebarItems.map((item) => (
          <button
            key={item.key}
            onClick={() => {
              if (activeSidebarView === item.key && primarySidebarVisible) {
                // Clic sulla voce già attiva → chiude il pannello (toggle)
                setPrimarySidebarVisible(false);
              } else {
                setActiveSidebarView(item.key);
                setPrimarySidebarVisible(true);
              }
            }}
            title={item.label}
            aria-label={item.label}
            style={{
              width: activityButtonSize,
              height: activityButtonSize,
              borderRadius: 8,
              border: `1px solid ${activeSidebarView === item.key ? tc.accent : tc.border}`,
              background: activeSidebarView === item.key ? tc.accentBg : "transparent",
              color: activeSidebarView === item.key ? tc.accent : tc.textSecondary,
              cursor: "pointer",
              fontWeight: 700,
              fontSize: 14,
            }}
          >
            {item.icon}
          </button>
        ))}

        {/* Spacer per spingere il menu utente in fondo */}
        <div style={{ flex: 1 }} />

        {/* Menu utente in fondo alla activity bar */}
        <UserSidebarMenu buttonSize={activityButtonSize} tc={tc} />
      </aside>

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
        <div
          style={{
            display: "flex",
            alignItems: "center",
            borderBottom: `1px solid ${tc.border}`,
            background: tc.bgHeader,
            overflowX: "auto",
          }}
        >
          {panelTabs.map((tab) => (
            <PanelTabButton
              key={tab.key}
              tab={tab}
              active={activePanelTab === tab.key}
              tc={tc}
              isMobileViewport={isMobileViewport}
              onSelect={() => setActivePanelTab(tab.key)}
            />
          ))}
          <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 8, paddingRight: 12 }}>
            {activeProject && <QuotaBadge projectId={activeProject.id} />}
            <button
              type="button"
              onClick={() => setBottomPanelVisible(false)}
              title="Nascondi panel"
              aria-label="Nascondi panel"
              style={iconButton(tc)}
            >
              ✕
            </button>
          </div>
        </div>
        <div style={{ minHeight: 0, overflow: "hidden" }}>
          <BottomPanelManager
            activePanelTab={activePanelTab}
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
            }}
            onAutoSendToChat={(msg) => {
              setPendingChatMessage(msg);
              setPendingAutoSend(true);
              setPendingExternalAutomation("confirm");
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
      <footer
        style={{
          gridColumn: "1 / 5",
          gridRow: "4",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "0 10px",
          borderTop: `1px solid ${tc.border}`,
          background: tc.bgHeader,
          color: tc.textMuted,
          fontSize: 11,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <span>{currentBranch}</span>
          <span>{activeProject?.name ?? "nessun progetto"}</span>
          <span>{layoutMode}</span>
          <span>{problemCount} problemi</span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <span>UTF-8</span>
          <span>LF</span>
          <span title={liveHealth.database ? "Database online" : "Database offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
            <StatusDot ok={liveHealth.database} />
            DB
          </span>
          <span title={liveHealth.redis ? "Redis online" : "Redis offline"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
            <StatusDot ok={liveHealth.redis} />
            Redis
          </span>
          <span title={
            liveHealth.neural_core && liveHealth.brain_rest
              ? "Brain (Python LangGraph) online — gRPC + REST ok"
              : !liveHealth.neural_core && !liveHealth.brain_rest
                ? "Brain offline — gRPC e REST irraggiungibili"
                : !liveHealth.brain_rest
                  ? "Brain REST (:8001) offline — gli agent run non funzioneranno"
                  : "Brain gRPC (:50051) offline — la chat potrebbe non rispondere"
          } style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
            <StatusDot ok={liveHealth.neural_core && !!liveHealth.brain_rest} />
            Brain
          </span>
          <span title={liveHealth.tools_grpc ? "MCP Tools (gRPC :50071) online" : "MCP Tools offline — l'AI non potrà eseguire tool (read_file, str_replace, ecc.)"} style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
            <StatusDot ok={!!liveHealth.tools_grpc} />
            Tools
          </span>
        </div>
      </footer>

      {/* ── Overlays ──────────────────────────────────────────────────────── */}
      {projectBusy && (
        <div
          style={{
            position: "fixed",
            top: 12,
            right: 12,
            padding: "8px 12px",
            borderRadius: 8,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            color: tc.text,
            fontSize: 12,
          }}
        >
          Caricamento progetto...
        </div>
      )}

      {projectError && (
        <div
          style={{
            position: "fixed",
            bottom: 36,
            right: 12,
            maxWidth: 520,
            padding: "8px 12px",
            borderRadius: 8,
            background: `${tc.error}18`,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            fontSize: 12,
            zIndex: 10,
          }}
        >
          {projectError}
        </div>
      )}

      {/* Banner Brain offline — visibile e prominente */}
      {(!liveHealth.neural_core || !liveHealth.brain_rest) && (
        <div
          style={{
            position: "fixed",
            top: 38,
            left: "50%",
            transform: "translateX(-50%)",
            padding: "8px 20px",
            borderRadius: 8,
            background: "#dc2626",
            color: "#fff",
            fontSize: 13,
            fontWeight: 600,
            zIndex: 9999,
            display: "flex",
            alignItems: "center",
            gap: 8,
            boxShadow: "0 4px 12px rgba(220,38,38,0.4)",
          }}
        >
          <span style={{ fontSize: 16 }}>!</span>
          {!liveHealth.neural_core && !liveHealth.brain_rest
            ? "Brain offline — la chat e gli agent run non funzioneranno"
            : !liveHealth.brain_rest
              ? "Brain REST offline — gli agent run non funzioneranno"
              : "Brain gRPC offline — la chat potrebbe non rispondere"}
        </div>
      )}
      <ToastStack />
    </main>
  );
}

/**
 * Singolo tab del PanelDock con highlight effect quando dispatcher emette
 * HighlightPanel per la sua key.
 */
function PanelTabButton({
  tab,
  active,
  tc,
  isMobileViewport,
  onSelect,
}: {
  tab: { key: PanelTab; label: string };
  active: boolean;
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  onSelect: () => void;
}) {
  const highlighted = usePanelHighlight(tab.key);
  return (
    <button
      onClick={onSelect}
      style={{
        border: "none",
        borderRight: `1px solid ${tc.border}`,
        background: highlighted
          ? "rgba(245,158,11,0.25)"
          : active
            ? tc.bg
            : "transparent",
        color: active ? tc.text : tc.textMuted,
        padding: isMobileViewport ? "0 8px" : "0 14px",
        height: "100%",
        cursor: "pointer",
        fontSize: isMobileViewport ? 11 : 12,
        whiteSpace: "nowrap",
        flexShrink: 0,
        transition: "background-color 200ms ease-out",
        boxShadow: highlighted ? "inset 0 -2px 0 #f59e0b" : "none",
      }}
    >
      {tab.label}
    </button>
  );
}

// ── Tab bar pannello destro: switcha tra Editor (file Monaco) e SQL (pannello
// gestore query). Vedi listener `nexus:sql:open` in ide-shell che imposta
// rightView="sql" su richiesta dalla chat.
function RightViewTabs({
  rightView,
  setRightView,
  tc,
}: {
  rightView: "editor" | "sql";
  setRightView: (v: "editor" | "sql") => void;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const Tab = ({
    label,
    active,
    onClick,
  }: {
    label: string;
    active: boolean;
    onClick: () => void;
  }) => (
    <button
      type="button"
      onClick={onClick}
      style={{
        padding: "0 12px",
        height: "100%",
        background: active ? tc.bgActive : "transparent",
        color: active ? tc.text : tc.textMuted,
        border: "none",
        borderRight: `1px solid ${tc.border}`,
        cursor: "pointer",
        fontSize: 12,
        fontWeight: active ? 600 : 400,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </button>
  );
  return (
    <div
      style={{
        display: "flex",
        alignItems: "stretch",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
        fontSize: 12,
      }}
    >
      <Tab label="Editor" active={rightView === "editor"} onClick={() => setRightView("editor")} />
      <Tab label="SQL" active={rightView === "sql"} onClick={() => setRightView("sql")} />
    </div>
  );
}
