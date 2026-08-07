"use client";

import dynamic from "next/dynamic";
import type { ReactNode } from "react";
import type { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails } from "../../lib/api-client";
import { RightViewTabs } from "./panel-tabs";
import { useI18n } from "../../lib/i18n";

// Stesso dynamic import che viveva in ide-shell: il pannello SQL e' pesante e va
// caricato solo quando la linguetta lo seleziona, non nel bundle dello shell.
const SqlQueryPanel = dynamic(
  () => import("../sql/sql-query-panel").then((m) => m.SqlQueryPanel),
  {
    loading: () => (
      <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", fontSize: 12 }}>
        …
      </div>
    ),
    ssr: false,
  },
);

/**
 * Cornice unica della colonna destra dell'area centrale: le linguette Editor/SQL
 * e il pannello che la linguetta seleziona.
 *
 * Punto unico (regola L): la scelta "quale vista mostra la colonna destra"
 * (rightView) e le sue linguette vivevano SOLO nel layout ai-center. In
 * editor-center e split-ai-editor la colonna rendeva l'editor nudo, ignorando
 * rightView: il pannello SQL — pur montabile via bridge nexus:sql:open dalla chat
 * — restava irraggiungibile, e chi ci finiva non aveva la linguetta per tornare
 * all'editor. I tre layout ora delegano qui invece di duplicare la cornice.
 */
export function RightColumn({
  rightView,
  setRightView,
  tc,
  project,
  editor,
}: {
  rightView: "editor" | "sql";
  setRightView: (v: "editor" | "sql") => void;
  tc: ReturnType<typeof useThemeColors>;
  project: UserProjectDetails | null;
  editor: ReactNode;
}) {
  const { t } = useI18n();
  return (
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
      <RightViewTabs rightView={rightView} setRightView={setRightView} tc={tc} />
      {rightView === "editor" ? editor : <SqlQueryPanel project={project} />}
    </div>
  );
}
