"use client";

import { useCallback, useEffect, useState } from "react";
import {
  getMyProjects,
  getVectorCompactionRuns,
  runVectorCompaction,
  type UserProjectSummary,
  type VectorCompactionRun,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";

export default function AdminVectorMaintenancePage() {
  const tc = useThemeColors();
  const [runs, setRuns] = useState<VectorCompactionRun[]>([]);
  const [projects, setProjects] = useState<UserProjectSummary[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string>("");
  const [busy, setBusy] = useState<"refresh" | "run" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastSummary, setLastSummary] = useState<Record<string, unknown> | null>(null);

  const refresh = useCallback(async () => {
    setBusy("refresh");
    setError(null);
    try {
      const [runResponse, projectResponse] = await Promise.all([
        getVectorCompactionRuns(100),
        getMyProjects(),
      ]);
      setRuns(runResponse.runs ?? []);
      setProjects(projectResponse.projects ?? []);
      if (!selectedProjectId && projectResponse.projects?.length) {
        setSelectedProjectId(projectResponse.projects[0].id);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Impossibile caricare i dati vector maintenance.");
    } finally {
      setBusy(null);
    }
  }, [selectedProjectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const runCompactionNow = async (projectScoped: boolean) => {
    setBusy("run");
    setError(null);
    try {
      const response = await runVectorCompaction(projectScoped ? selectedProjectId : undefined);
      setLastSummary(response.summary);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Compattazione fallita.");
    } finally {
      setBusy(null);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div>
        <h1 style={{ fontSize: 20, fontWeight: 600, marginBottom: 6 }}>Vector Maintenance</h1>
        <p style={{ color: tc.textMuted, fontSize: 13, margin: 0 }}>
          Compattazione vettoriale, metriche before/after e storico run.
        </p>
      </div>

      {error && (
        <div
          style={{
            padding: "10px 14px",
            borderRadius: 8,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            background: tc.bgCard,
            fontSize: 13,
          }}
        >
          {error}
        </div>
      )}

      <div
        style={{
          padding: 14,
          borderRadius: 10,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          gap: 8,
          alignItems: "center",
          flexWrap: "wrap",
        }}
      >
        <select
          value={selectedProjectId}
          onChange={(e) => setSelectedProjectId(e.target.value)}
          style={inputStyle(tc)}
        >
          {projects.map((project) => (
            <option key={project.id} value={project.id}>
              {project.name}
            </option>
          ))}
        </select>
        <button
          onClick={() => void runCompactionNow(true)}
          style={buttonStyle(tc)}
          disabled={busy === "run" || !selectedProjectId}
        >
          {busy === "run" ? "Eseguo..." : "Compatta progetto"}
        </button>
        <button
          onClick={() => void runCompactionNow(false)}
          style={buttonStyle(tc)}
          disabled={busy === "run"}
        >
          {busy === "run" ? "Eseguo..." : "Compatta globale"}
        </button>
        <button onClick={() => void refresh()} style={buttonStyle(tc)} disabled={busy === "refresh"}>
          {busy === "refresh" ? "Aggiorno..." : "Aggiorna storico"}
        </button>
      </div>

      {lastSummary && (
        <div
          style={{
            padding: 14,
            borderRadius: 10,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
          }}
        >
          <div style={{ fontSize: 13, marginBottom: 8 }}>Ultimo run manuale</div>
          <pre style={{ margin: 0, fontSize: 12, color: tc.textSecondary, whiteSpace: "pre-wrap" }}>
            {JSON.stringify(lastSummary, null, 2)}
          </pre>
        </div>
      )}

      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {runs.map((run) => (
          <div
            key={run.id}
            style={{
              padding: 14,
              borderRadius: 10,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              display: "grid",
              gap: 6,
            }}
          >
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              {run.startedAt} • trigger: {run.triggerType} • status: {run.status}
            </div>
            <div style={{ fontSize: 13 }}>
              before: <strong>{run.beforeCount}</strong> • after: <strong>{run.afterCount}</strong> • deleted:{" "}
              <strong>{run.deletedCount}</strong> • dedup: <strong>{run.dedupCount}</strong>
            </div>
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              qdrantDeleted: {run.qdrantDeletedCount} • project: {run.projectId ?? "all"}
            </div>
          </div>
        ))}
        {runs.length === 0 && (
          <div
            style={{
              padding: 14,
              borderRadius: 10,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              color: tc.textMuted,
              fontSize: 13,
            }}
          >
            Nessun run disponibile.
          </div>
        )}
      </div>
    </div>
  );
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    minWidth: 260,
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontSize: 12,
    fontFamily: "inherit",
  } as const;
}

function buttonStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    padding: "8px 12px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "inherit",
  } as const;
}
