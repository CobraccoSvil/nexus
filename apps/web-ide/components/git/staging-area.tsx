"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  type GitFileChange,
  getGitDiff,
  getGitUiPreferences,
  stageGitPaths,
  type UserProjectDetails,
  updateGitUiPreferences,
  unstageGitPaths,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import {
  parseUnifiedDiff,
  renderSplitDiff,
  renderUnifiedDiff,
} from "./diff-utils";
import { useI18n } from "../../lib/i18n";

function buttonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
  return {
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgCard : tc.accentBg,
    color: tc.text,
    cursor: disabled ? "not-allowed" : "pointer",
  };
}

const KIND_COLOR: Record<string, string> = {
  modified: "#f97316",
  added: "#22c55e",
  deleted: "#ef4444",
  renamed: "#3b82f6",
  untracked: "#a78bfa",
};
const KIND_LETTER: Record<string, string> = {
  modified: "M", added: "A", deleted: "D", renamed: "R", untracked: "U",
};

function renderChanges(
  title: string,
  items: GitFileChange[],
  tc: ReturnType<typeof useThemeColors>,
  project: UserProjectDetails,
  busy: boolean,
  runAction: (action: () => Promise<unknown>) => Promise<void>,
  staged: boolean,
  onShowDiff: (path: string, staged: boolean) => Promise<void>,
  selectedPath?: string,
) {
  if (items.length === 0) return null;

  return (
    <div style={{ marginBottom: 4 }}>
      {/* Section header */}
      <div style={{
        display: "flex", alignItems: "center", justifyContent: "space-between",
        padding: "2px 6px", marginBottom: 1,
      }}>
        <span style={{ fontSize: 10, fontWeight: 700, color: tc.textMuted, textTransform: "uppercase", letterSpacing: "0.06em" }}>
          {title} ({items.length})
        </span>
        {project.canManageGit && (
          <button
            disabled={busy}
            onClick={() => runAction(() =>
              staged
                ? unstageGitPaths(project.id, items.map(i => i.path))
                : stageGitPaths(project.id, items.map(i => i.path))
            )}
            title={staged ? "Rimuovi tutti dallo stage" : "Stage tutti"}
            style={{ background: "none", border: "none", color: tc.textMuted, fontSize: 11, cursor: busy ? "not-allowed" : "pointer", padding: "0 2px" }}
          >
            {staged ? "－All" : "＋All"}
          </button>
        )}
      </div>
      {/* File rows */}
      {items.map((item) => {
        const kindKey = item.kind?.toLowerCase() ?? "modified";
        const letter = KIND_LETTER[kindKey] ?? "M";
        const color = KIND_COLOR[kindKey] ?? tc.textMuted;
        const isSelected = selectedPath === item.path;
        const filename = item.path.split("/").pop() ?? item.path;
        const dir = item.path.includes("/") ? item.path.slice(0, item.path.lastIndexOf("/")) : "";
        return (
          <div
            key={`${title}-${item.path}`}
            onClick={() => void onShowDiff(item.path, staged)}
            title={item.path}
            style={{
              display: "flex", alignItems: "center", gap: 4,
              padding: "2px 6px", cursor: "pointer",
              background: isSelected ? `${tc.accent}18` : "transparent",
              borderRadius: 3,
            }}
            onMouseEnter={e => { if (!isSelected) (e.currentTarget as HTMLDivElement).style.background = `${tc.bgInput}`; }}
            onMouseLeave={e => { if (!isSelected) (e.currentTarget as HTMLDivElement).style.background = "transparent"; }}
          >
            {/* Kind letter */}
            <span style={{ fontSize: 10, fontWeight: 700, color, flexShrink: 0, width: 12, textAlign: "center" }}>{letter}</span>
            {/* Filename + dir */}
            <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              <span style={{ fontSize: 12, color: tc.text }}>{filename}</span>
              {dir && <span style={{ fontSize: 10, color: tc.textMuted, marginLeft: 4 }}>{dir}</span>}
            </span>
            {/* Action button */}
            {project.canManageGit && (
              <button
                disabled={busy}
                onClick={e => { e.stopPropagation(); void runAction(() =>
                  staged ? unstageGitPaths(project.id, [item.path]) : stageGitPaths(project.id, [item.path])
                ); }}
                title={staged ? "Rimuovi dallo stage" : "Aggiungi allo stage"}
                style={{ background: "none", border: "none", color: tc.textMuted, fontSize: 13, cursor: busy ? "not-allowed" : "pointer", padding: "0 2px", flexShrink: 0, lineHeight: 1 }}
              >
                {staged ? "－" : "＋"}
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

interface StagingAreaProps {
  project: UserProjectDetails;
  staged: GitFileChange[];
  unstaged: GitFileChange[];
  untracked: GitFileChange[];
  busy: boolean;
  runAction: (action: () => Promise<unknown>) => Promise<void>;
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>;
}

export function StagingArea({
  project,
  staged,
  unstaged,
  untracked,
  busy,
  runAction,
  onOpenFileAtLine,
}: StagingAreaProps) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const [selectedDiff, setSelectedDiff] = useState<{ path: string; staged: boolean } | null>(null);
  const [diffPreview, setDiffPreview] = useState("");
  const [diffViewMode, setDiffViewMode] = useState<"unified" | "split">("split");
  const [activeHunk, setActiveHunk] = useState<number>(-1);
  const [showHunkMap, setShowHunkMap] = useState(true);
  const [diffBusy, setDiffBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parsedDiff = useMemo(() => parseUnifiedDiff(diffPreview), [diffPreview]);
  const hunks = useMemo(
    () =>
      parsedDiff
        .filter((line) => line.isHunkHeader)
        .map((line) => ({ index: line.hunkIndex, label: line.content })),
    [parsedDiff],
  );

  const goPrevHunk = useCallback(() => {
    setActiveHunk((current) => Math.max(0, current - 1));
  }, []);
  const goNextHunk = useCallback(() => {
    setActiveHunk((current) => Math.min(hunks.length - 1, current + 1));
  }, [hunks.length]);

  useEffect(() => {
    if (hunks.length === 0) {
      setActiveHunk(-1);
      return;
    }
    setActiveHunk(0);
  }, [diffPreview, hunks.length]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (hunks.length === 0) return;
      if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;

      const target = event.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName.toLowerCase();
        const editable =
          tag === "input" ||
          tag === "textarea" ||
          tag === "select" ||
          target.isContentEditable;
        if (editable) return;
      }

      if (event.key === "[") {
        event.preventDefault();
        goPrevHunk();
      } else if (event.key === "]") {
        event.preventDefault();
        goNextHunk();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [goNextHunk, goPrevHunk, hunks.length]);

  useEffect(() => {
    const loadPreferences = async () => {
      if (!project?.isGitRepo) return;
      try {
        const preferences = await getGitUiPreferences(project.id);
        setShowHunkMap(preferences.showHunkMap);
      } catch {
        setShowHunkMap(true);
      }
    };
    void loadPreferences();
  }, [project?.id, project?.isGitRepo]);

  const toggleHunkMap = async () => {
    if (!project?.isGitRepo) return;
    const nextValue = !showHunkMap;
    setShowHunkMap(nextValue);
    try {
      await updateGitUiPreferences(project.id, nextValue);
    } catch (nextError) {
      setShowHunkMap(!nextValue);
      setError(nextError instanceof Error ? nextError.message : "Impossibile salvare la preferenza");
    }
  };

  const loadDiff = async (path: string, stagedFile: boolean) => {
    if (!project?.isGitRepo) return;
    try {
      setDiffBusy(true);
      setError(null);
      setSelectedDiff({ path, staged: stagedFile });
      const response = await getGitDiff(project.id, path, stagedFile);
      setDiffPreview(response.diff);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "Impossibile caricare la diff");
    } finally {
      setDiffBusy(false);
    }
  };

  // Refresh diff when runAction completes (external trigger via selectedDiff)
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const loadDiffIfSelected = useCallback(async () => {
    if (!selectedDiff || !project?.isGitRepo) return;
    try {
      const response = await getGitDiff(project.id, selectedDiff.path, selectedDiff.staged);
      setDiffPreview(response.diff);
    } catch {
      // ignore
    }
  }, [project?.id, project?.isGitRepo, selectedDiff]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, minWidth: 0, width: "100%" }}>
      <div style={{ minWidth: 0 }}>
        <div style={{ color: tc.textSecondary, marginBottom: 6, fontSize: 11, fontWeight: 600 }}>{t("git.modifiche")}</div>
        {renderChanges("Staged", staged, tc, project, busy, runAction, true, loadDiff, selectedDiff?.path)}
        {renderChanges("Unstaged", unstaged, tc, project, busy, runAction, false, loadDiff, selectedDiff?.path)}
        {renderChanges("Untracked", untracked, tc, project, busy, runAction, false, loadDiff, selectedDiff?.path)}
        {staged.length === 0 && unstaged.length === 0 && untracked.length === 0 && (
          <div style={{ color: tc.textMuted, fontSize: 12, padding: "4px 6px" }}>{t("git.nessunaModifica")}</div>
        )}
      </div>

      <div>
        {/* Header con titolo e info */}
        <div style={{ color: tc.textSecondary, marginBottom: 8, display: "flex", alignItems: "center", gap: 8, justifyContent: "space-between", minWidth: 0, flexWrap: "nowrap" }}>
          <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1 }}>
            Diff preview
            {selectedDiff ? ` - ${selectedDiff.path}` : ""}
          </span>
          {hunks.length > 0 && (
            <span style={{ fontSize: 11, color: tc.textMuted, flexShrink: 0, whiteSpace: "nowrap" }}>
              Hunk {activeHunk + 1}/{hunks.length}
            </span>
          )}
        </div>
        {/* Bottoni in riga responsive */}
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
          <button
            disabled={!selectedDiff}
            title={showHunkMap ? "Nascondi mappa hunks" : "Mostra mappa hunks"}
            onClick={() => {
              void toggleHunkMap();
            }}
            style={{
              ...buttonStyle(tc, !selectedDiff),
              padding: "5px 8px",
              flexShrink: 0,
            }}
          >
            {showHunkMap ? "▲" : "▼"}
          </button>
          <button
            disabled={diffBusy || hunks.length === 0 || activeHunk <= 0}
            onClick={goPrevHunk}
            title={t("git.hunkPrecedente")}
            style={{
              ...buttonStyle(tc, diffBusy || hunks.length === 0 || activeHunk <= 0),
              padding: "5px 8px",
              flexShrink: 0,
            }}
          >
            ←
          </button>
          <button
            disabled={diffBusy || hunks.length === 0 || activeHunk >= hunks.length - 1}
            onClick={goNextHunk}
            title={t("git.hunkSuccessivo")}
            style={{
              ...buttonStyle(tc, diffBusy || hunks.length === 0 || activeHunk >= hunks.length - 1),
              padding: "5px 8px",
              flexShrink: 0,
            }}
          >
            →
          </button>
          <button
            disabled={diffBusy || !selectedDiff}
            onClick={() => setDiffViewMode("split")}
            title={t("git.vistaSplitColonneAffiancate")}
            style={{
              ...buttonStyle(tc, diffBusy || !selectedDiff),
              background: diffViewMode === "split" ? tc.accentBg : tc.bgCard,
              padding: "5px 8px",
              flexShrink: 0,
            }}
          >
            ⊞
          </button>
          <button
            disabled={diffBusy || !selectedDiff}
            onClick={() => setDiffViewMode("unified")}
            title={t("git.vistaUnificataSequenziale")}
            style={{
              ...buttonStyle(tc, diffBusy || !selectedDiff),
              background: diffViewMode === "unified" ? tc.accentBg : tc.bgCard,
              padding: "5px 8px",
              flexShrink: 0,
            }}
          >
            ☰
          </button>
        </div>
        {showHunkMap && hunks.length > 0 && (
          <div
            style={{
              border: `1px solid ${tc.border}`,
              borderRadius: 8,
              background: tc.bgCard,
              padding: 8,
              marginBottom: 8,
              display: "flex",
              gap: 6,
              overflowX: "auto",
            }}
          >
            {hunks.map((hunk) => {
              const isActive = hunk.index === activeHunk;
              return (
                <button
                  key={`${hunk.index}-${hunk.label}`}
                  onClick={() => setActiveHunk(hunk.index)}
                  style={{
                    ...buttonStyle(tc, false),
                    background: isActive ? tc.accentBg : tc.bgInput,
                    borderColor: isActive ? tc.accent : tc.border,
                    color: isActive ? tc.accent : tc.textSecondary,
                    padding: "4px 8px",
                    whiteSpace: "nowrap",
                  }}
                  title={hunk.label}
                >
                  H{hunk.index + 1}
                </button>
              );
            })}
          </div>
        )}
        <div
          style={{
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            background: tc.bgInput,
            padding: 10,
            minHeight: 120,
            maxHeight: 260,
            overflow: "auto",
            fontFamily: "var(--font-mono)",
            fontSize: 12,
            lineHeight: 1.5,
            color: tc.text,
          }}
        >
          {diffBusy
            ? "Caricamento diff..."
            : selectedDiff
              ? diffPreview
                ? diffViewMode === "split"
                  ? renderSplitDiff(parsedDiff, tc, activeHunk, selectedDiff.path, onOpenFileAtLine)
                  : renderUnifiedDiff(parsedDiff, tc, activeHunk, selectedDiff.path, onOpenFileAtLine)
                : "Nessuna diff disponibile (file non tracciato o nessuna modifica testuale)."
              : "Seleziona un file nella sezione Modifiche per vedere la diff."}
        </div>
      </div>

      {error && <div style={{ color: tc.error }}>{error}</div>}
    </div>
  );
}
