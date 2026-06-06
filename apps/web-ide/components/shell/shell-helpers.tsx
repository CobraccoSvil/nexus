"use client";

import type {
  EditorGroupState,
  EditorTabState,
  WorkbenchLayoutMode,
  WorkbenchState,
} from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import type { SidebarView } from "../sidebar/sidebar-manager";
import type { PanelTab } from "../panels/bottom-panel-manager";

export type SecondarySidebarView = "ai-tools";
export type ProviderKey = "openai" | "anthropic" | "google" | "deepseek" | "mistral";
export type ProviderHealthState = {
  ok: boolean | null;
  reason?: string;
  status?: string;
  billing?: boolean; // true = crediti/quota esauriti → pallino giallo
};

export const sidebarItems: Array<{ key: SidebarView; label: string; icon: string }> = [
  { key: "project-db", label: "Database", icon: "🗄" },
  { key: "knowledge", label: "Knowledge", icon: "🧠" },
  { key: "explorer", label: "Explorer", icon: "🗂" },
  { key: "search", label: "Ricerca", icon: "🔍" },
  { key: "source-control", label: "Git", icon: "⑂" },
  { key: "run", label: "Run", icon: "▶" },
  { key: "docs", label: "Documenti", icon: "📄" },
  { key: "mutations", label: "Modifiche", icon: "↶" },
  { key: "server-monitor", label: "Monitor", icon: "▣" },
];

export const panelTabs: Array<{ key: PanelTab; label: string }> = [
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

export const EMPTY_GROUPS: EditorGroupState[] = [
  { id: "primary", tabs: [], activePath: null },
  { id: "secondary", tabs: [], activePath: null },
];

export function basename(path: string) {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function makeTab(path: string, content: string, dirty = false): EditorTabState {
  return {
    path,
    title: basename(path),
    dirty,
    pinned: true,
    content,
  };
}

export function defaultWorkbenchState(): WorkbenchState {
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

export function normalizeWorkbenchState(input?: Partial<WorkbenchState> | null): WorkbenchState {
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

export async function hydrateGroups(
  projectId: string,
  state: WorkbenchState,
  fallbackPaths: string[],
  getProjectFile: (projectId: string, path: string) => Promise<{ path: string; content: string }>,
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

export function StatusDot({ ok, billing }: { ok: boolean | null; billing?: boolean }) {
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

export function summarizeProviderReason(reason?: string): string | undefined {
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

export function providerTitle(label: string, state: ProviderHealthState): string {
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

export function iconButton(tc: ReturnType<typeof useThemeColors>, disabled = false, active = false) {
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
