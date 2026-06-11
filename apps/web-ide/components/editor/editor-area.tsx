"use client";

import dynamic from "next/dynamic";
import { useEffect, useRef } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import { iconButton } from "../../lib/icon-button-style";
import type { EditorGroupState, EditorTabState, ProblemItem, UserProjectDetails } from "../../lib/api-client";
import type * as Monaco from "monaco-editor";
import { MarkdownBlock } from "../chat/markdown-renderer";

function isMarkdownPath(path: string): boolean {
  const lower = path.toLowerCase();
  return lower.endsWith(".md") || lower.endsWith(".markdown");
}

export interface EditorAreaProps {
  editorGroups: EditorGroupState[];
  activeEditorGroupId: string;
  activeProject: UserProjectDetails | null;
  problemItems?: ProblemItem[];
  onSetActiveGroup: (id: string) => void;
  onSetEditorGroups: (updater: (current: EditorGroupState[]) => EditorGroupState[]) => void;
  onSaveActive: () => void;
  onRenameActive: () => void;
  onDeleteActive: () => void;
  onConfirmCloseTab: (tab: EditorTabState) => Promise<boolean> | boolean;
}

const MonacoEditor = dynamic(
  async () => (await import("@monaco-editor/react")).default,
  { ssr: false },
);

function detectMonacoLanguage(path: string): string {
  const normalized = path.toLowerCase();
  const ext = normalized.includes(".")
    ? normalized.split(".").pop() ?? ""
    : "";

  if (normalized.endsWith(".d.ts")) return "typescript";

  const map: Record<string, string> = {
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    json: "json",
    md: "markdown",
    markdown: "markdown",
    yml: "yaml",
    yaml: "yaml",
    css: "css",
    scss: "scss",
    less: "less",
    html: "html",
    xml: "xml",
    py: "python",
    rs: "rust",
    go: "go",
    java: "java",
    kt: "kotlin",
    cs: "csharp",
    cpp: "cpp",
    cc: "cpp",
    cxx: "cpp",
    c: "c",
    h: "cpp",
    hpp: "cpp",
    sql: "sql",
    sh: "shell",
    bash: "shell",
    ps1: "powershell",
    toml: "ini",
    ini: "ini",
    env: "shell",
    dockerfile: "dockerfile",
    gql: "graphql",
    graphql: "graphql",
    php: "php",
    rb: "ruby",
    swift: "swift",
    vue: "html",
    svelte: "html",
    txt: "plaintext",
  };

  if (normalized.endsWith("dockerfile")) return "dockerfile";
  return map[ext] ?? "plaintext";
}

// Map ProblemItem severity → Monaco MarkerSeverity number
function toMarkerSeverity(severity: string): number {
  const s = severity.toLowerCase();
  if (s === "error" || s === "critical") return 8;   // Error
  if (s === "high") return 8;                         // Error
  if (s === "warning" || s === "medium") return 4;    // Warning
  if (s === "info" || s === "low") return 2;          // Info
  return 1;                                           // Hint
}

// Normalize a file path for comparison (forward slashes, no leading slash)
function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/^\/+/, "").toLowerCase();
}

export function EditorArea({
  editorGroups,
  activeEditorGroupId,
  activeProject,
  problemItems = [],
  onSetActiveGroup,
  onSetEditorGroups,
  onSaveActive,
  onRenameActive,
  onDeleteActive,
  onConfirmCloseTab,
}: EditorAreaProps) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const monacoRef = useRef<typeof Monaco | null>(null);
  const editorRefs = useRef<Record<string, { getAction?: (id: string) => { run: () => Promise<void> } | null } | null>>({});

  // Sync diagnostic markers whenever problemItems or open tabs change
  useEffect(() => {
    const monacoInstance = monacoRef.current;
    if (!monacoInstance) return;

    const models = monacoInstance.editor.getModels();
    // Clear all nexus markers first
    for (const model of models) {
      monacoInstance.editor.setModelMarkers(model, "nexus", []);
    }

    if (problemItems.length === 0) return;

    // Group problems by normalized file path
    const byPath = new Map<string, ProblemItem[]>();
    for (const item of problemItems) {
      if (!item.filePath) continue;
      const key = normalizePath(item.filePath);
      const group = byPath.get(key) ?? [];
      group.push(item);
      byPath.set(key, group);
    }

    for (const model of models) {
      // model.uri is something like "file:///src/app.ts" or just the path
      const uriPath = normalizePath(model.uri.path);
      const matchingProblems = byPath.get(uriPath) ?? [];
      if (matchingProblems.length === 0) {
        // Try suffix match (filePath might be relative while uri is absolute)
        for (const [probPath, items] of byPath) {
          if (uriPath.endsWith("/" + probPath) || uriPath === probPath) {
            matchingProblems.push(...items);
          }
        }
      }
      if (matchingProblems.length === 0) continue;

      const markers: Monaco.editor.IMarkerData[] = matchingProblems.map((item) => ({
        severity: toMarkerSeverity(item.severity),
        message: `[${item.source}] ${item.message}`,
        startLineNumber: item.line ?? 1,
        startColumn: item.column ?? 1,
        endLineNumber: item.line ?? 1,
        endColumn: (item.column ?? 80),
        source: item.source,
      }));

      monacoInstance.editor.setModelMarkers(model, "nexus", markers);
    }
  }, [problemItems, editorGroups]);

  // Sync Monaco theme when IDE theme changes
  useEffect(() => {
    const monacoInstance = monacoRef.current;
    if (!monacoInstance) return;
    monacoInstance.editor.setTheme(resolved === "dark" ? "vs-dark" : "vs");
  }, [resolved]);

  const activeGroup =
    editorGroups.find((g) => g.id === activeEditorGroupId) ?? editorGroups[0];
  const activeEditorTab =
    activeGroup?.tabs.find((t) => t.path === activeGroup.activePath) ?? null;

  const handleFormatActive = () => {
    if (!activeGroup) return;
    const editor = editorRefs.current[activeGroup.id];
    const action = editor?.getAction?.("editor.action.formatDocument");
    if (action) {
      void action.run();
    }
  };

  const closeTab = async (groupId: string, tab: EditorTabState) => {
    const canClose = tab.dirty ? await onConfirmCloseTab(tab) : true;
    if (!canClose) return;
    onSetEditorGroups((current) =>
      current.map((group) => {
        if (group.id !== groupId) return group;
        const tabs = group.tabs.filter((t) => t.path !== tab.path);
        const activePath =
          group.activePath === tab.path
            ? tabs[tabs.length - 1]?.path ?? null
            : group.activePath;
        return { ...group, tabs, activePath };
      }),
    );
  };

  const updateContent = (groupId: string, path: string, content: string) => {
    onSetEditorGroups((current) =>
      current.map((group) => {
        if (group.id !== groupId) return group;
        return {
          ...group,
          tabs: group.tabs.map((t) =>
            t.path === path ? { ...t, content, dirty: true } : t,
          ),
          activePath: path,
        };
      }),
    );
  };

  const renderGroup = (group: EditorGroupState, label: string, index: number) => {
    const activePath = group.activePath;
    const activeTab = group.tabs.find((t) => t.path === activePath) ?? null;

    return (
      <div
        key={group.id}
        style={{
          minWidth: 0,
          minHeight: 0,
          height: "100%",
          display: "grid",
          gridTemplateRows: "36px minmax(0, 1fr)",
          borderLeft: index === 0 ? "none" : `1px solid ${tc.border}`,
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "stretch",
            background: tc.bgHeader,
            borderBottom: `1px solid ${tc.border}`,
            overflowX: "auto",
          }}
        >
          <div
            style={{
              padding: "0 8px 0 8px",
              display: "flex",
              alignItems: "center",
              color: tc.textMuted,
              borderRight: `1px solid ${tc.border}`,
              fontSize: 11,
              flexShrink: 0,
            }}
          >
            {label}
          </div>
          {group.tabs.map((tab) => (
            <div
              key={`${group.id}-${tab.path}`}
              onClick={() => {
                onSetActiveGroup(group.id);
                onSetEditorGroups((current) =>
                  current.map((item) =>
                    item.id === group.id
                      ? { ...item, activePath: tab.path }
                      : item,
                  ),
                );
              }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "0 10px",
                borderRight: `1px solid ${tc.border}`,
                background: tab.path === activePath ? tc.bg : tc.bgHeader,
                color:
                  tab.path === activePath ? tc.text : tc.textMuted,
                cursor: "pointer",
                flexShrink: 0,
              }}
            >
              <span>{tab.title}</span>
              {tab.dirty && (
                <span style={{ color: tc.warning }}>●</span>
              )}
              <span
                onClick={(e) => {
                  e.stopPropagation();
                  void closeTab(group.id, tab);
                }}
                style={{ opacity: 0.6 }}
              >
                x
              </span>
            </div>
          ))}
          {/* Azioni a destra: Doc (Code Wiki) del file + toggle markdown */}
          {activeTab && (
            <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", padding: "0 8px", gap: 4 }}>
              <button
                onClick={() =>
                  window.dispatchEvent(
                    new CustomEvent("nexus:kb:open-code-doc", {
                      detail: { filePath: activeTab.path },
                    }),
                  )
                }
                title="Apri la documentazione (Code Wiki) di questo file"
                style={{
                  padding: "3px 10px",
                  fontSize: 11,
                  borderRadius: 4,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgCard,
                  color: "#7c3aed",
                  cursor: "pointer",
                }}
              >
                Doc
              </button>
              {isMarkdownPath(activeTab.path) &&
                (["source", "preview"] as const).map((mode) => {
                const current = activeTab.viewMode ?? "preview";
                const isActive = current === mode;
                return (
                  <button
                    key={mode}
                    onClick={() => {
                      onSetEditorGroups((curr) =>
                        curr.map((g) =>
                          g.id === group.id
                            ? {
                                ...g,
                                tabs: g.tabs.map((t) =>
                                  t.path === activeTab.path ? { ...t, viewMode: mode } : t,
                                ),
                              }
                            : g,
                        ),
                      );
                    }}
                    style={{
                      padding: "3px 10px",
                      fontSize: 11,
                      borderRadius: 4,
                      border: `1px solid ${isActive ? tc.accent : tc.border}`,
                      background: isActive ? tc.accentBg : tc.bgCard,
                      color: isActive ? tc.accent : tc.textSecondary,
                      cursor: "pointer",
                    }}
                  >
                    {mode === "source" ? "Sorgente" : "Anteprima"}
                  </button>
                );
              })}
            </div>
          )}
        </div>
        <div style={{ minHeight: 0, height: "100%", background: tc.bg }}>
          {activeTab ? (
            isMarkdownPath(activeTab.path) && (activeTab.viewMode ?? "preview") === "preview" ? (
              <div
                style={{
                  height: "100%",
                  minHeight: 0,
                  overflow: "auto",
                  background: tc.bg,
                  color: tc.text,
                }}
              >
                <div
                  style={{
                    maxWidth: 900,
                    margin: "0 auto",
                    padding: "24px 32px 60px",
                  }}
                >
                  <MarkdownBlock content={activeTab.content ?? ""} skipNormalize />
                </div>
              </div>
            ) : (
            <div style={{ height: "100%", minHeight: 0 }}>
              <MonacoEditor
                path={activeTab.path}
                language={detectMonacoLanguage(activeTab.path)}
                value={activeTab.content ?? ""}
                beforeMount={(monaco) => {
                  // Must be set BEFORE models are created (beforeMount, not onMount).
                  // Disables semantic validation (type checking) which produces false-positive
                  // "module not found" errors — Monaco runs without tsconfig/node_modules.
                  // Real compilation errors come via problemItems → setModelMarkers.
                  const diagOpts = {
                    noSemanticValidation: true,
                    noSyntaxValidation: false,
                    noSuggestionDiagnostics: true,
                  };
                  monaco.languages.typescript.typescriptDefaults.setDiagnosticsOptions(diagOpts);
                  monaco.languages.typescript.javascriptDefaults.setDiagnosticsOptions(diagOpts);
                  monaco.languages.typescript.typescriptDefaults.setCompilerOptions({
                    ...monaco.languages.typescript.typescriptDefaults.getCompilerOptions(),
                    moduleResolution: monaco.languages.typescript.ModuleResolutionKind.NodeJs,
                    allowSyntheticDefaultImports: true,
                    esModuleInterop: true,
                    jsx: monaco.languages.typescript.JsxEmit.ReactJSX,
                    noEmit: true,
                    strict: false,
                  });
                }}
                onMount={(editor, monaco) => {
                  monacoRef.current = monaco as unknown as typeof Monaco;
                  editorRefs.current[group.id] = editor;
                  editor.onDidFocusEditorWidget?.(() => onSetActiveGroup(group.id));
                  monaco.editor.setTheme(resolved === "dark" ? "vs-dark" : "vs");
                }}
                onChange={(value) => updateContent(group.id, activeTab.path, value ?? "")}
                theme={resolved === "dark" ? "vs-dark" : "vs"}
                options={{
                  readOnly: !activeProject?.canWrite,
                  automaticLayout: true,
                  minimap: { enabled: false },
                  fontSize: 13,
                  lineHeight: 20,
                  fontFamily: '"JetBrains Mono", monospace',
                  scrollBeyondLastLine: false,
                  wordWrap: "off",
                  tabSize: 2,
                  formatOnPaste: true,
                  formatOnType: true,
                  smoothScrolling: true,
                  bracketPairColorization: { enabled: true },
                  guides: { indentation: true, bracketPairs: true },
                }}
              />
            </div>
            )
          ) : (
            <div style={{ padding: 20, color: tc.textMuted, fontSize: 13 }}>
              Apri un file per iniziare.
            </div>
          )}
        </div>
      </div>
    );
  };

  const visibleGroups = editorGroups.filter((g) => g.tabs.length > 0);
  const groupsToRender =
    visibleGroups.length > 0 ? visibleGroups : [editorGroups[0]];

  return (
    <div
      style={{
        display: "grid",
        gridTemplateRows: "32px minmax(0, 1fr)",
        minHeight: 0,
        height: "100%",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "0 12px 0 8px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgHeader,
        }}
      >
        <div style={{ color: tc.text, fontSize: 13, fontWeight: 700 }}>
          Editor Workspace
        </div>
        <div style={{ display: "flex", gap: 6 }}>
          <button
            type="button"
            onClick={handleFormatActive}
            title="Formatta documento"
            aria-label="Formatta documento"
            style={iconButton(tc, !activeEditorTab)}
          >
            ↹
          </button>
          <button
            type="button"
            onClick={onSaveActive}
            title="Salva file attivo"
            aria-label="Salva file attivo"
            style={iconButton(
              tc,
              !activeEditorTab ||
                !activeEditorTab.dirty ||
                !activeProject?.canWrite,
            )}
          >
            💾
          </button>
          <button
            type="button"
            onClick={onRenameActive}
            title="Rinomina file attivo"
            aria-label="Rinomina file attivo"
            style={iconButton(tc, !activeEditorTab || !activeProject?.canWrite)}
          >
            ✎
          </button>
          <button
            type="button"
            onClick={onDeleteActive}
            title="Elimina file attivo"
            aria-label="Elimina file attivo"
            style={iconButton(tc, !activeEditorTab || !activeProject?.canWrite)}
          >
            🗑
          </button>
        </div>
      </div>
      <div
        style={{
          minHeight: 0,
          height: "100%",
          display: "grid",
          gridTemplateColumns:
            groupsToRender.length > 1 ? "1fr 1fr" : "1fr",
        }}
      >
        {groupsToRender.map((group, index) => renderGroup(group, `Gruppo ${index + 1}`, index))}
      </div>
    </div>
  );
}
