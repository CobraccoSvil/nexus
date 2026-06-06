"use client";
import { useThemeColors } from "../../lib/theme";
import { shortenAbsolutePath } from "../../lib/format";
import { DocumentsSidebar } from "./documents-sidebar";
import { ProjectExplorer } from "../project-explorer";
import { SourceControlPanel } from "../git/source-control-panel";
import { ServerMonitorPanel } from "./server-monitor-panel";
import { ProjectDbPanel } from "../project-db/project-db-panel";
import { KnowledgeNavigator } from "./knowledge-navigator";
import { iconButton, inputStyle, listRowButton } from "./manager/shared";
import { ViewHeader } from "./manager/view-header";
import { RunDebugView } from "./manager/run-debug-view";
import type {
  EditorTabState,
  GitRepositoryState,
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
  | "project-db"
  | "knowledge";

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
  /** Chiamato dall'Explorer dopo create/rename/delete: il parent ricarica metadata progetto. */
  onFileTreeChanged?: () => void;
  /** Chiamato dall'Explorer dopo delete: il parent chiude tab editor aperti su quel path. */
  onFileDeleted?: (path: string) => void;
  /** Chiamato dall'Explorer dopo rename/move: il parent aggiorna tab editor (oldPath -> newPath). */
  onFileRenamed?: (oldPath: string, newPath: string) => void;
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
  onFileTreeChanged,
  onFileDeleted,
  onFileRenamed,
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
            onFileTreeChanged={onFileTreeChanged}
            onFileDeleted={onFileDeleted}
            onFileRenamed={onFileRenamed}
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
        onOpenInEditor={(relativePath) => onOpenFile(relativePath)}
      />
    );
  }

  if (activeSidebarView === "knowledge" && project) {
    return (
      <div style={{ flex: 1, minHeight: 0, height: "100%", overflow: "hidden", display: "flex", flexDirection: "column", minWidth: 0 }}>
        <a
          href={`/projects/${project.id}/kb`}
          style={{
            flexShrink: 0,
            padding: "6px 10px",
            background: `${tc.accent}1a`,
            borderBottom: `1px solid ${tc.border}`,
            color: tc.accent,
            fontSize: 11,
            textDecoration: "none",
            display: "block",
            lineHeight: 1.3,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
          title="Apri la Knowledge Base unificata a tutto schermo"
        >
          Knowledge Base unificata · <strong>Apri a schermo intero →</strong>
        </a>
        <KnowledgeNavigator projectId={project.id} />
      </div>
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
