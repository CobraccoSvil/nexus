import { API_BASE, fetchJson } from "./_shared";

export interface WorkspaceTreeNode {
  name: string;
  path: string;
  kind: "directory" | "file";
  hasChildren: boolean;
}

export interface ProjectFileBuffer {
  path: string;
  content: string;
}

export type WorkbenchLayoutMode = "ai-center" | "editor-center" | "split-ai-editor";

export interface EditorTabState {
  path: string;
  title: string;
  dirty: boolean;
  pinned: boolean;
  content?: string;
  /** Vista corrente del tab: "source" (Monaco) o "preview" (rendering markdown).
   *  Significativo solo per file .md/.markdown; default "source". */
  viewMode?: "source" | "preview";
}

export interface EditorGroupState {
  id: string;
  tabs: EditorTabState[];
  activePath?: string | null;
}

export interface WorkbenchState {
  layoutMode: WorkbenchLayoutMode;
  primarySidebarVisible: boolean;
  secondarySidebarVisible?: boolean;
  secondarySidebarView?: string;
  layoutControlStyle?: "icon-menu";
  iconButtonsOnly?: boolean;
  bottomPanelVisible: boolean;
  activeSidebarView: string;
  activePanelTab: string;
  leftWidth: number;
  rightWidth: number;
  bottomHeight: number;
  editorGroups: EditorGroupState[];
  ai: {
    activeContextPaths: string[];
  };
  terminal: {
    activeTabId?: string | null;
    tabs: Array<{ id: string; title: string }>;
  };
  chat?: {
    provider: string;
    model: string;
    automationMode: "study" | "confirm" | "automatic";
  };
}

export interface SearchResultItem {
  path: string;
  line: number;
  column: number;
  preview: string;
}

export interface ProblemItem {
  id: string;
  severity: string;
  source: string;
  message: string;
  filePath?: string;
  line?: number;
  column?: number;
  createdAt: string;
}

export interface OutputChannel {
  id: string;
  label: string;
  title?: string;   // tooltip esteso (es. nome unit completo)
  kind?: "service" | "task";
}

export interface OutputEvent {
  id: string;
  channel: string;
  level: string;
  title: string;
  text: string;
  createdAt: string;
}

export async function getWorkbenchState(
  projectId: string,
): Promise<{ state: Partial<WorkbenchState>; session: { activeFilePaths: string[]; terminalCwd?: string; updatedAt?: string } }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/workbench-state`);
}

export async function updateWorkbenchState(
  projectId: string,
  state: Partial<WorkbenchState>,
  activeFilePaths: string[] = [],
  terminalCwd?: string,
): Promise<{ ok: boolean; state: Partial<WorkbenchState> }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/workbench-state`, {
    method: "PUT",
    body: JSON.stringify({
      state,
      activeFilePaths,
      terminalCwd,
    }),
  });
}

export async function getProjectTree(
  projectId: string,
  path = "",
): Promise<{ path: string; nodes: WorkspaceTreeNode[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/tree`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  if (path) url.searchParams.set("path", path);
  return fetchJson(url.toString());
}

export async function getProjectFile(projectId: string, path: string): Promise<ProjectFileBuffer> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/files`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("path", path);
  return fetchJson(url.toString());
}

export async function saveProjectFile(
  projectId: string,
  path: string,
  content: string,
): Promise<{ saved: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files`, {
    method: "PUT",
    body: JSON.stringify({ path, content }),
  });
}

export async function createProjectEntry(
  projectId: string,
  path: string,
  kind: "file" | "directory",
  content?: string,
): Promise<{ ok: boolean; path: string; kind: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/create`, {
    method: "POST",
    body: JSON.stringify({ path, kind, content }),
  });
}

export async function renameProjectEntry(
  projectId: string,
  oldPath: string,
  newPath: string,
): Promise<{ ok: boolean; oldPath: string; newPath: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/rename`, {
    method: "POST",
    body: JSON.stringify({ old_path: oldPath, new_path: newPath }),
  });
}

export async function deleteProjectEntry(
  projectId: string,
  path: string,
): Promise<{ ok: boolean; path: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/files/delete`, {
    method: "POST",
    body: JSON.stringify({ path }),
  });
}

export async function searchProject(
  projectId: string,
  query: string,
  limit = 100,
): Promise<{ query: string; results: SearchResultItem[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/search`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("q", query);
  url.searchParams.set("limit", String(limit));
  return fetchJson(url.toString());
}

export async function getProjectProblems(
  projectId: string,
): Promise<{ items: ProblemItem[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/problems`);
}

export async function getOutputChannels(
  projectId: string,
): Promise<{ channels: OutputChannel[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/output/channels`);
}

export async function getOutputEvents(
  projectId: string,
  channel: string,
  limit = 100,
): Promise<{ channel: string; events: OutputEvent[] }> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/output/events`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("channel", channel);
  url.searchParams.set("limit", String(limit));
  return fetchJson(url.toString());
}
