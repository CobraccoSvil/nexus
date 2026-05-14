"use client";

import { Fragment, useEffect, useMemo, useState } from "react";
import { getProjectTree, type UserProjectDetails, type WorkspaceTreeNode } from "../lib/api-client";
import { useThemeColors } from "../lib/theme";
import { shortenAbsolutePath } from "../lib/format";

type TreeMap = Record<string, WorkspaceTreeNode[]>;

export function ProjectExplorer({
  project,
  initialNodes,
  activeFilePath,
  onOpenFile,
}: {
  project?: UserProjectDetails | null;
  initialNodes?: WorkspaceTreeNode[];
  activeFilePath?: string | null;
  onOpenFile: (path: string) => Promise<void>;
}) {
  const tc = useThemeColors();
  const [nodesByPath, setNodesByPath] = useState<TreeMap>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [loadingPaths, setLoadingPaths] = useState<Record<string, boolean>>({});

  useEffect(() => {
    if (!project) {
      setNodesByPath({});
      setExpanded({});
      return;
    }
    setNodesByPath({ "": initialNodes ?? [] });
    setExpanded({ "": true });
  }, [project, project?.id, initialNodes]);

  const loadPath = async (path: string) => {
    if (!project || loadingPaths[path]) return;
    if (nodesByPath[path]) return;

    setLoadingPaths((prev) => ({ ...prev, [path]: true }));
    try {
      const response = await getProjectTree(project.id, path);
      setNodesByPath((prev) => ({ ...prev, [path]: response.nodes }));
    } finally {
      setLoadingPaths((prev) => ({ ...prev, [path]: false }));
    }
  };

  const toggleDirectory = async (path: string) => {
    setExpanded((prev) => ({ ...prev, [path]: !prev[path] }));
    if (!nodesByPath[path]) {
      await loadPath(path);
    }
  };

  const renderNodes = (path: string, depth: number) => {
    const items = nodesByPath[path] ?? [];
    return items.map((node) => {
      const isDirectory = node.kind === "directory";
      const isExpanded = !!expanded[node.path];
      const isActive = activeFilePath === node.path;
      return (
        <Fragment key={node.path}>
          <button
            onClick={() => {
              if (isDirectory) {
                void toggleDirectory(node.path);
              } else {
                void onOpenFile(node.path);
              }
            }}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              width: "100%",
              padding: `6px 8px 6px ${12 + depth * 14}px`,
              borderRadius: 8,
              border: "none",
              background: isActive ? tc.accentBg : "transparent",
              color: isActive ? tc.accent : tc.text,
              cursor: "pointer",
              textAlign: "left",
            }}
          >
            <span style={{ width: 14 }}>
              {isDirectory ? (isExpanded ? "v" : ">") : "•"}
            </span>
            <span>{node.name}</span>
          </button>
          {isDirectory && isExpanded && (
            <div>
              {loadingPaths[node.path] && (
                <div style={{ paddingLeft: 24 + depth * 14, color: tc.textMuted, fontSize: 12 }}>
                  Caricamento...
                </div>
              )}
              {renderNodes(node.path, depth + 1)}
            </div>
          )}
        </Fragment>
      );
    });
  };

  const emptyMessage = useMemo(() => {
    if (!project) return "Registra o apri un progetto per iniziare.";
    return "Nessun file disponibile nella directory selezionata.";
  }, [project]);

  return (
    <div style={{ fontSize: 13 }}>
      {project?.rootPath && (
        <div
          style={{ color: tc.textMuted, marginBottom: 10, wordBreak: "break-all" }}
          title={project.rootPath}
        >
          {/* Fix M8: mostra path accorciato (es. ~/projects/myslug) invece di
             leakare /home/administrator/ideai/projects/... — full path nel title */}
          {shortenAbsolutePath(project.rootPath, project.rootPath)}
        </div>
      )}
      {(nodesByPath[""]?.length ?? 0) === 0 ? (
        <div className="text-muted">{emptyMessage}</div>
      ) : (
        <div>{renderNodes("", 0)}</div>
      )}
    </div>
  );
}

