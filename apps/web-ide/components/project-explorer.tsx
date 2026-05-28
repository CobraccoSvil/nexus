"use client";

import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createProjectEntry,
  deleteProjectEntry,
  getProjectTree,
  renameProjectEntry,
  type UserProjectDetails,
  type WorkspaceTreeNode,
} from "../lib/api-client";
import { useThemeColors } from "../lib/theme";
import { shortenAbsolutePath } from "../lib/format";

type TreeMap = Record<string, WorkspaceTreeNode[]>;

type ContextMenuState = {
  x: number;
  y: number;
  node: WorkspaceTreeNode;
} | null;

// Modal dialog inline (non-blocking) per sostituire window.confirm/prompt/alert.
// I dialog nativi del browser bloccano i test automatici (Chrome MCP / Playwright
// driver via CDP) e hanno una UX inconsistente col resto della UI. Qui usiamo
// un dialog modale custom che restituisce una Promise.
type ModalState =
  | {
      kind: "alert";
      title: string;
      message: string;
      resolve: (value: void) => void;
    }
  | {
      kind: "confirm";
      title: string;
      message: string;
      danger?: boolean;
      confirmLabel?: string;
      resolve: (value: boolean) => void;
    }
  | {
      kind: "prompt";
      title: string;
      label: string;
      defaultValue?: string;
      placeholder?: string;
      resolve: (value: string | null) => void;
    }
  | null;

export function ProjectExplorer({
  project,
  initialNodes,
  activeFilePath,
  onOpenFile,
  onFileTreeChanged,
  onFileDeleted,
  onFileRenamed,
}: {
  project?: UserProjectDetails | null;
  initialNodes?: WorkspaceTreeNode[];
  activeFilePath?: string | null;
  onOpenFile: (path: string) => Promise<void>;
  /** Chiamato dopo create/rename/delete: il parent ricarica i metadata del progetto. */
  onFileTreeChanged?: () => void;
  /** Chiamato dopo delete: il parent chiude eventuali tab aperti su quel path. */
  onFileDeleted?: (path: string) => void;
  /** Chiamato dopo rename: il parent aggiorna eventuali tab aperti (oldPath -> newPath). */
  onFileRenamed?: (oldPath: string, newPath: string) => void;
}) {
  const tc = useThemeColors();
  const [nodesByPath, setNodesByPath] = useState<TreeMap>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [loadingPaths, setLoadingPaths] = useState<Record<string, boolean>>({});
  const [contextMenu, setContextMenu] = useState<ContextMenuState>(null);
  const [modal, setModal] = useState<ModalState>(null);
  const [actionInFlight, setActionInFlight] = useState(false);

  // Helper Promise-based per dialog modali. Sostituiscono window.confirm/prompt/alert.
  const confirmDialog = useCallback(
    (opts: { title: string; message: string; danger?: boolean; confirmLabel?: string }) =>
      new Promise<boolean>((resolve) => {
        setModal({ kind: "confirm", resolve, ...opts });
      }),
    [],
  );
  const promptDialog = useCallback(
    (opts: { title: string; label: string; defaultValue?: string; placeholder?: string }) =>
      new Promise<string | null>((resolve) => {
        setModal({ kind: "prompt", resolve, ...opts });
      }),
    [],
  );
  const alertDialog = useCallback(
    (opts: { title: string; message: string }) =>
      new Promise<void>((resolve) => {
        setModal({ kind: "alert", resolve, ...opts });
      }),
    [],
  );

  useEffect(() => {
    if (!project) {
      setNodesByPath({});
      setExpanded({});
      return;
    }
    setNodesByPath({ "": initialNodes ?? [] });
    setExpanded({ "": true });
  }, [project, project?.id, initialNodes]);

  // Fix M36: polling lieve sulla root ogni 8s per rilevare nuovi file/dir
  // generati dall'agente (write_file via gRPC ToolRunner). Senza questo
  // l'Explorer restava con la snapshot on-mount e l'utente vedeva
  // "Nessun file disponibile" anche se l'agente aveva appena scritto file.
  // Confronto su lunghezza + lista nomi: se diversi -> ricarica e invalida
  // anche le subdir gia caricate (loaded paths) per allinearle.
  useEffect(() => {
    if (!project?.id) return;
    const refreshRoot = async () => {
      try {
        const response = await getProjectTree(project.id, "");
        setNodesByPath((prev) => {
          const current = prev[""] ?? [];
          const changed =
            current.length !== response.nodes.length ||
            current.some((n, i) => n.path !== response.nodes[i]?.path);
          if (!changed) return prev;
          // Invalida le subdir per forzare reload al prossimo expand
          const next: TreeMap = { "": response.nodes };
          return next;
        });
      } catch {
        // Best-effort: ignora errori transitori (auth/network)
      }
    };
    const interval = window.setInterval(refreshRoot, 8_000);
    return () => window.clearInterval(interval);
  }, [project?.id]);

  // Ascolta evento dispatcher FileChanged per refresh immediato dopo op.
  // Pattern: tool_delete_file / write_file in mcp-core emettono ProjectEvent::FileChanged
  // che il dispatcher SSE propaga al frontend. Qui ascoltiamo il custom event
  // bridge `nexus:explorer:refresh` che ide-shell rilancia dal dispatcher store.
  useEffect(() => {
    if (!project?.id) return;
    const handler = () => {
      void refreshAll();
    };
    window.addEventListener("nexus:explorer:refresh", handler);
    return () => window.removeEventListener("nexus:explorer:refresh", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project?.id]);

  const refreshAll = useCallback(async () => {
    if (!project?.id) return;
    try {
      const response = await getProjectTree(project.id, "");
      // Invalida tutte le subdir gia' caricate: il prossimo expand le ricaricava.
      setNodesByPath({ "": response.nodes });
      onFileTreeChanged?.();
    } catch {
      // ignora
    }
  }, [project?.id, onFileTreeChanged]);

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

  // ── Operazioni file (handler context menu) ─────────────────────────────
  const closeContextMenu = useCallback(() => setContextMenu(null), []);

  const handleRename = useCallback(async (node: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id) return;
    const defaultName = node.name;
    const newName = await promptDialog({
      title: `Rinomina "${node.name}"`,
      label: "Nuovo nome",
      defaultValue: defaultName,
      placeholder: defaultName,
    });
    if (!newName || newName.trim() === "" || newName === defaultName) return;
    // newPath = parent + "/" + newName
    const parentParts = node.path.split("/").slice(0, -1);
    const newPath = [...parentParts, newName.trim()].join("/");
    setActionInFlight(true);
    try {
      await renameProjectEntry(project.id, node.path, newPath);
      onFileRenamed?.(node.path, newPath);
      await refreshAll();
    } catch (err) {
      await alertDialog({
        title: "Rinomina fallita",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, onFileRenamed, closeContextMenu, promptDialog, alertDialog]);

  const handleMove = useCallback(async (node: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id) return;
    const newPath = await promptDialog({
      title: `Sposta "${node.path}"`,
      label: "Nuovo path relativo",
      defaultValue: node.path,
      placeholder: `src/util/${node.name}`,
    });
    if (!newPath || newPath.trim() === "" || newPath === node.path) return;
    setActionInFlight(true);
    try {
      await renameProjectEntry(project.id, node.path, newPath.trim());
      onFileRenamed?.(node.path, newPath.trim());
      await refreshAll();
    } catch (err) {
      await alertDialog({
        title: "Spostamento fallito",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, onFileRenamed, closeContextMenu, promptDialog, alertDialog]);

  const handleDelete = useCallback(async (node: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id) return;
    const label = node.kind === "directory" ? "la cartella" : "il file";
    const confirmed = await confirmDialog({
      title: `Elimina ${label}`,
      message:
        `Vuoi davvero eliminare ${label} "${node.path}"?` +
        (node.kind === "directory" ? "\n\nVerranno cancellati TUTTI i file al suo interno." : "") +
        "\n\nOperazione irreversibile.",
      danger: true,
      confirmLabel: "Elimina",
    });
    if (!confirmed) return;
    setActionInFlight(true);
    try {
      await deleteProjectEntry(project.id, node.path);
      onFileDeleted?.(node.path);
      await refreshAll();
    } catch (err) {
      await alertDialog({
        title: "Cancellazione fallita",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, onFileDeleted, closeContextMenu, confirmDialog, alertDialog]);

  const handleDuplicate = useCallback(async (node: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id || node.kind === "directory") return;
    // Calcola newPath: aggiungi suffix prima dell'estensione (es. foo.txt → foo-copia.txt)
    const lastDot = node.name.lastIndexOf(".");
    const dupName = lastDot > 0
      ? `${node.name.slice(0, lastDot)}-copia${node.name.slice(lastDot)}`
      : `${node.name}-copia`;
    const parentParts = node.path.split("/").slice(0, -1);
    const newPath = [...parentParts, dupName].join("/");
    setActionInFlight(true);
    try {
      // Per duplicare un file, leggi il contenuto e create con stesso content
      const { getProjectFile } = await import("../lib/api-client");
      const content = await getProjectFile(project.id, node.path);
      await createProjectEntry(project.id, newPath, "file", content.content ?? "");
      await refreshAll();
    } catch (err) {
      await alertDialog({
        title: "Duplicazione fallita",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, closeContextMenu, alertDialog]);

  const handleCopyPath = useCallback(async (node: WorkspaceTreeNode) => {
    closeContextMenu();
    try {
      await navigator.clipboard.writeText(node.path);
    } catch {
      // Fallback: mostra il path in un dialog read-only se la clipboard API non e' disponibile
      await alertDialog({
        title: "Copia manuale",
        message: `Clipboard API non disponibile. Path:\n\n${node.path}`,
      });
    }
  }, [closeContextMenu, alertDialog]);

  const handleNewFileInDir = useCallback(async (dirNode: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id) return;
    const name = await promptDialog({
      title: `Nuovo file in "${dirNode.path}"`,
      label: "Nome file",
      defaultValue: "nuovo-file.txt",
      placeholder: "nuovo-file.txt",
    });
    if (!name || name.trim() === "") return;
    const newPath = `${dirNode.path}/${name.trim()}`;
    setActionInFlight(true);
    try {
      await createProjectEntry(project.id, newPath, "file", "");
      // Auto-espandi la dir parent + apri il nuovo file
      setExpanded((prev) => ({ ...prev, [dirNode.path]: true }));
      await refreshAll();
      await onOpenFile(newPath);
    } catch (err) {
      await alertDialog({
        title: "Creazione fallita",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, onOpenFile, closeContextMenu, promptDialog, alertDialog]);

  const handleNewDirInDir = useCallback(async (dirNode: WorkspaceTreeNode) => {
    closeContextMenu();
    if (!project?.id) return;
    const name = await promptDialog({
      title: `Nuova cartella in "${dirNode.path}"`,
      label: "Nome cartella",
      defaultValue: "nuova-cartella",
      placeholder: "nuova-cartella",
    });
    if (!name || name.trim() === "") return;
    const newPath = `${dirNode.path}/${name.trim()}`;
    setActionInFlight(true);
    try {
      await createProjectEntry(project.id, newPath, "directory");
      setExpanded((prev) => ({ ...prev, [dirNode.path]: true }));
      await refreshAll();
    } catch (err) {
      await alertDialog({
        title: "Creazione cartella fallita",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setActionInFlight(false);
    }
  }, [project?.id, refreshAll, closeContextMenu, promptDialog, alertDialog]);

  // ── Render ──────────────────────────────────────────────────────────────
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
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ x: e.clientX, y: e.clientY, node });
            }}
            disabled={actionInFlight}
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
              cursor: actionInFlight ? "wait" : "pointer",
              textAlign: "left",
              opacity: actionInFlight ? 0.6 : 1,
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

      {/* Context menu */}
      {contextMenu && (
        <ExplorerContextMenu
          node={contextMenu.node}
          x={contextMenu.x}
          y={contextMenu.y}
          canWrite={!!project?.canWrite}
          onClose={closeContextMenu}
          onOpen={async (n) => { closeContextMenu(); if (n.kind === "file") await onOpenFile(n.path); }}
          onRename={handleRename}
          onMove={handleMove}
          onDelete={handleDelete}
          onDuplicate={handleDuplicate}
          onCopyPath={handleCopyPath}
          onNewFileInDir={handleNewFileInDir}
          onNewDirInDir={handleNewDirInDir}
        />
      )}

      {/* Modal dialog (sostituisce window.confirm/prompt/alert) */}
      {modal && (
        <ExplorerModal
          state={modal}
          onResolveAlert={() => {
            if (modal.kind === "alert") modal.resolve();
            setModal(null);
          }}
          onResolveConfirm={(value) => {
            if (modal.kind === "confirm") modal.resolve(value);
            setModal(null);
          }}
          onResolvePrompt={(value) => {
            if (modal.kind === "prompt") modal.resolve(value);
            setModal(null);
          }}
        />
      )}
    </div>
  );
}


/** Context menu (right-click) per nodi dell'Explorer.
 *
 * Voci dinamiche in base a kind del nodo:
 *  - file: Apri, Rinomina, Duplica, Sposta, Copia path, Cancella
 *  - directory: Nuovo file, Nuova cartella, Rinomina, Sposta, Copia path, Cancella
 *
 * Click fuori = chiudi. Tasto ESC = chiudi. Posizionamento via fixed.
 * Auto-clamp dentro viewport per evitare overflow.
 */
function ExplorerContextMenu({
  node,
  x,
  y,
  canWrite,
  onClose,
  onOpen,
  onRename,
  onMove,
  onDelete,
  onDuplicate,
  onCopyPath,
  onNewFileInDir,
  onNewDirInDir,
}: {
  node: WorkspaceTreeNode;
  x: number;
  y: number;
  canWrite: boolean;
  onClose: () => void;
  onOpen: (n: WorkspaceTreeNode) => void | Promise<void>;
  onRename: (n: WorkspaceTreeNode) => void | Promise<void>;
  onMove: (n: WorkspaceTreeNode) => void | Promise<void>;
  onDelete: (n: WorkspaceTreeNode) => void | Promise<void>;
  onDuplicate: (n: WorkspaceTreeNode) => void | Promise<void>;
  onCopyPath: (n: WorkspaceTreeNode) => void | Promise<void>;
  onNewFileInDir: (n: WorkspaceTreeNode) => void | Promise<void>;
  onNewDirInDir: (n: WorkspaceTreeNode) => void | Promise<void>;
}) {
  const tc = useThemeColors();
  const menuRef = useRef<HTMLDivElement>(null);

  // Click fuori + ESC = chiudi
  useEffect(() => {
    const onClick = (ev: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(ev.target as Node)) {
        onClose();
      }
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onClick);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onClick);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // Clamp dentro viewport
  const safeX = Math.min(x, (typeof window !== "undefined" ? window.innerWidth : 1920) - 220);
  const safeY = Math.min(y, (typeof window !== "undefined" ? window.innerHeight : 1080) - 320);

  const isDirectory = node.kind === "directory";

  const itemStyle: React.CSSProperties = {
    padding: "6px 12px",
    cursor: "pointer",
    fontSize: 13,
    color: tc.text,
    border: "none",
    background: "transparent",
    textAlign: "left",
    width: "100%",
    display: "block",
  };
  const separatorStyle: React.CSSProperties = {
    height: 1,
    background: tc.border,
    margin: "4px 0",
  };
  const dangerStyle: React.CSSProperties = {
    ...itemStyle,
    color: "#f87171",
  };

  return (
    <div
      ref={menuRef}
      role="menu"
      style={{
        position: "fixed",
        top: safeY,
        left: safeX,
        zIndex: 9999,
        minWidth: 200,
        background: tc.bg ?? "#1f1f1f",
        border: `1px solid ${tc.border}`,
        borderRadius: 8,
        boxShadow: "0 8px 24px rgba(0,0,0,0.4)",
        padding: "4px 0",
        userSelect: "none",
      }}
    >
      {!isDirectory && (
        <button style={itemStyle} onClick={() => void onOpen(node)} role="menuitem">
          Apri
        </button>
      )}
      {isDirectory && (
        <>
          <button
            style={canWrite ? itemStyle : { ...itemStyle, opacity: 0.5, cursor: "not-allowed" }}
            onClick={() => canWrite && void onNewFileInDir(node)}
            role="menuitem"
            disabled={!canWrite}
          >
            Nuovo file qui...
          </button>
          <button
            style={canWrite ? itemStyle : { ...itemStyle, opacity: 0.5, cursor: "not-allowed" }}
            onClick={() => canWrite && void onNewDirInDir(node)}
            role="menuitem"
            disabled={!canWrite}
          >
            Nuova cartella qui...
          </button>
          <div style={separatorStyle} />
        </>
      )}
      <button
        style={canWrite ? itemStyle : { ...itemStyle, opacity: 0.5, cursor: "not-allowed" }}
        onClick={() => canWrite && void onRename(node)}
        role="menuitem"
        disabled={!canWrite}
      >
        Rinomina...
      </button>
      <button
        style={canWrite ? itemStyle : { ...itemStyle, opacity: 0.5, cursor: "not-allowed" }}
        onClick={() => canWrite && void onMove(node)}
        role="menuitem"
        disabled={!canWrite}
      >
        Sposta...
      </button>
      {!isDirectory && (
        <button
          style={canWrite ? itemStyle : { ...itemStyle, opacity: 0.5, cursor: "not-allowed" }}
          onClick={() => canWrite && void onDuplicate(node)}
          role="menuitem"
          disabled={!canWrite}
        >
          Duplica
        </button>
      )}
      <div style={separatorStyle} />
      <button style={itemStyle} onClick={() => void onCopyPath(node)} role="menuitem">
        Copia path
      </button>
      <div style={separatorStyle} />
      <button
        style={canWrite ? dangerStyle : { ...dangerStyle, opacity: 0.5, cursor: "not-allowed" }}
        onClick={() => canWrite && void onDelete(node)}
        role="menuitem"
        disabled={!canWrite}
      >
        Cancella
      </button>
    </div>
  );
}


/** Modal dialog inline per alert/confirm/prompt.
 *
 * Sostituisce window.confirm/prompt/alert nativi che:
 *  - bloccano i driver di automazione (Chrome MCP, Playwright)
 *  - hanno uno stile diverso dal resto della UI
 *  - non gestiscono autofocus/ESC/Invio in modo coerente
 *
 * Resolve della Promise sottostante e' triggherato dal parent via callback.
 * ESC = cancel, Invio = OK (su prompt valida l'input).
 */
function ExplorerModal({
  state,
  onResolveAlert,
  onResolveConfirm,
  onResolvePrompt,
}: {
  state: NonNullable<ModalState>;
  onResolveAlert: () => void;
  onResolveConfirm: (value: boolean) => void;
  onResolvePrompt: (value: string | null) => void;
}) {
  const tc = useThemeColors();
  const inputRef = useRef<HTMLInputElement>(null);
  const okButtonRef = useRef<HTMLButtonElement>(null);
  const [inputValue, setInputValue] = useState<string>(
    state.kind === "prompt" ? state.defaultValue ?? "" : "",
  );

  // Autofocus + selezione testo per prompt; focus su OK per confirm/alert
  useEffect(() => {
    const t = window.setTimeout(() => {
      if (state.kind === "prompt") {
        inputRef.current?.focus();
        inputRef.current?.select();
      } else {
        okButtonRef.current?.focus();
      }
    }, 30);
    return () => window.clearTimeout(t);
  }, [state.kind]);

  // ESC = cancel/dismiss
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      if (state.kind === "alert") onResolveAlert();
      else if (state.kind === "confirm") onResolveConfirm(false);
      else if (state.kind === "prompt") onResolvePrompt(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [state.kind, onResolveAlert, onResolveConfirm, onResolvePrompt]);

  const handleOk = () => {
    if (state.kind === "alert") onResolveAlert();
    else if (state.kind === "confirm") onResolveConfirm(true);
    else if (state.kind === "prompt") onResolvePrompt(inputValue.trim() === "" ? null : inputValue);
  };
  const handleCancel = () => {
    if (state.kind === "confirm") onResolveConfirm(false);
    else if (state.kind === "prompt") onResolvePrompt(null);
    else onResolveAlert();
  };

  const isDanger = state.kind === "confirm" && state.danger === true;
  const confirmLabel =
    state.kind === "confirm" ? state.confirmLabel ?? "OK" : state.kind === "prompt" ? "Conferma" : "OK";

  // Overlay click = cancel (escluso quando click sul dialog box)
  return (
    <div
      role="presentation"
      onMouseDown={(ev) => {
        if (ev.target === ev.currentTarget) handleCancel();
      }}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 10000,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 16,
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="explorer-modal-title"
        onKeyDown={(ev) => {
          if (ev.key === "Enter" && state.kind !== "alert") {
            if (state.kind === "prompt" && ev.target instanceof HTMLInputElement) {
              ev.preventDefault();
              handleOk();
            } else if (state.kind === "confirm") {
              ev.preventDefault();
              handleOk();
            }
          }
        }}
        style={{
          minWidth: 360,
          maxWidth: 520,
          background: tc.bg ?? "#1f1f1f",
          color: tc.text,
          border: `1px solid ${tc.border}`,
          borderRadius: 10,
          boxShadow: "0 16px 48px rgba(0,0,0,0.5)",
          padding: 20,
          display: "flex",
          flexDirection: "column",
          gap: 14,
        }}
      >
        <div
          id="explorer-modal-title"
          style={{ fontSize: 15, fontWeight: 600, color: tc.text }}
        >
          {state.kind === "prompt" ? state.title : state.title}
        </div>

        {state.kind === "alert" && (
          <div style={{ whiteSpace: "pre-wrap", fontSize: 13, color: tc.textMuted }}>
            {state.message}
          </div>
        )}
        {state.kind === "confirm" && (
          <div style={{ whiteSpace: "pre-wrap", fontSize: 13, color: tc.textMuted }}>
            {state.message}
          </div>
        )}
        {state.kind === "prompt" && (
          <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <span style={{ fontSize: 12, color: tc.textMuted }}>{state.label}</span>
            <input
              ref={inputRef}
              type="text"
              value={inputValue}
              onChange={(ev) => setInputValue(ev.target.value)}
              placeholder={state.placeholder}
              style={{
                padding: "8px 10px",
                fontSize: 13,
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bg ?? "#0f0f0f",
                color: tc.text,
                outline: "none",
              }}
            />
          </label>
        )}

        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            marginTop: 4,
          }}
        >
          {state.kind !== "alert" && (
            <button
              type="button"
              onClick={handleCancel}
              style={{
                padding: "8px 14px",
                fontSize: 13,
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: "transparent",
                color: tc.text,
                cursor: "pointer",
              }}
            >
              Annulla
            </button>
          )}
          <button
            ref={okButtonRef}
            type="button"
            onClick={handleOk}
            data-testid="explorer-modal-ok"
            style={{
              padding: "8px 14px",
              fontSize: 13,
              borderRadius: 6,
              border: "none",
              background: isDanger ? "#dc2626" : (tc.accent ?? "#2563eb"),
              color: "#ffffff",
              cursor: "pointer",
              fontWeight: 600,
            }}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
