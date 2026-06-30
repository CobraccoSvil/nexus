"use client";

import { useEffect, useState } from "react";
import {
  getMyProjects,
  getProjectLearningConfig,
  retrainProjectRouting,
  updateProjectLearningConfig,
  type ProjectLearningConfig,
  type UserProjectSummary,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { useGlobalDialog } from "../../../components/global-dialog-provider";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";

const defaultConfig: ProjectLearningConfig = {
  enabled: true,
  promptCorrectionsEnabled: true,
  autoApplyMaxChangesPerDay: 2,
  feedbackThreshold: 5,
  feedbackWindowDays: 7,
  minConfidence: 0.65,
  rollbackWindowHours: 24,
};

export default function AdminProjectLearningPage() {
  const tc = useThemeColors();
  const { promptDialog } = useGlobalDialog();
  const [projects, setProjects] = useState<UserProjectSummary[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string>("");
  const [config, setConfig] = useState<ProjectLearningConfig>(defaultConfig);
  const [busy, setBusy] = useState<"load" | "save" | "retrain" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastDecision, setLastDecision] = useState<Record<string, unknown> | null>(null);

  useEffect(() => {
    const bootstrap = async () => {
      setBusy("load");
      setError(null);
      try {
        const response = await getMyProjects();
        setProjects(response.projects ?? []);
        if (response.projects?.length) {
          setSelectedProjectId(response.projects[0].id);
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : "Impossibile caricare progetti.");
      } finally {
        setBusy(null);
      }
    };
    void bootstrap();
  }, []);

  useEffect(() => {
    if (!selectedProjectId) return;
    const loadConfig = async () => {
      setBusy("load");
      setError(null);
      try {
        const response = await getProjectLearningConfig(selectedProjectId);
        setConfig(response.config);
      } catch (e) {
        setError(e instanceof Error ? e.message : "Impossibile caricare configurazione learning.");
      } finally {
        setBusy(null);
      }
    };
    void loadConfig();
  }, [selectedProjectId]);

  const saveConfig = async () => {
    if (!selectedProjectId) return;
    setBusy("save");
    setError(null);
    try {
      const response = await updateProjectLearningConfig(selectedProjectId, config);
      setConfig(response.config);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Salvataggio configurazione fallito.");
    } finally {
      setBusy(null);
    }
  };

  const retrain = async () => {
    if (!selectedProjectId) return;
    setBusy("retrain");
    setError(null);
    try {
      const intent =
        (await promptDialog(
          "Intent da retrain (default chat):",
          "chat",
          "Retrain routing",
        ))?.trim() || "chat";
      const response = await retrainProjectRouting(selectedProjectId, intent);
      setLastDecision(response.decision);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Retrain routing fallito.");
    } finally {
      setBusy(null);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <AdminPageHeader
        title="Project Learning"
        description="Configurazione auto-apprendimento per progetto e retrain manuale del routing."
      />

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

      <div style={panelStyle(tc)}>
        <label style={{ fontSize: 12, color: tc.textMuted }}>Progetto</label>
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
      </div>

      <div style={panelStyle(tc)}>
        <div style={{ display: "grid", gap: 10, gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))" }}>
          <label style={labelStyle(tc)}>
            <span>Learning attivo</span>
            <input
              type="checkbox"
              checked={config.enabled}
              onChange={(e) => setConfig((current) => ({ ...current, enabled: e.target.checked }))}
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Prompt corrections attive</span>
            <input
              type="checkbox"
              checked={config.promptCorrectionsEnabled}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  promptCorrectionsEnabled: e.target.checked,
                }))
              }
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Max cambi/giorno</span>
            <input
              type="number"
              min={0}
              value={config.autoApplyMaxChangesPerDay}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  autoApplyMaxChangesPerDay: Number(e.target.value || "0"),
                }))
              }
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Soglia feedback</span>
            <input
              type="number"
              min={1}
              value={config.feedbackThreshold}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  feedbackThreshold: Number(e.target.value || "1"),
                }))
              }
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Finestra feedback (giorni)</span>
            <input
              type="number"
              min={1}
              value={config.feedbackWindowDays}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  feedbackWindowDays: Number(e.target.value || "1"),
                }))
              }
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Min confidenza</span>
            <input
              type="number"
              step="0.01"
              min={0}
              max={1}
              value={config.minConfidence}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  minConfidence: Number(e.target.value || "0"),
                }))
              }
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            <span>Rollback window (ore)</span>
            <input
              type="number"
              min={1}
              value={config.rollbackWindowHours}
              onChange={(e) =>
                setConfig((current) => ({
                  ...current,
                  rollbackWindowHours: Number(e.target.value || "1"),
                }))
              }
              style={inputStyle(tc)}
            />
          </label>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button onClick={() => void saveConfig()} style={buttonStyle(tc)} disabled={busy === "save" || !selectedProjectId}>
            {busy === "save" ? "Salvo..." : "Salva configurazione"}
          </button>
          <button onClick={() => void retrain()} style={buttonStyle(tc)} disabled={busy === "retrain" || !selectedProjectId}>
            {busy === "retrain" ? "Eseguo..." : "Retrain routing"}
          </button>
        </div>
      </div>

      {lastDecision && (
        <div style={panelStyle(tc)}>
          <div style={{ fontSize: 13, marginBottom: 8 }}>Ultima decisione</div>
          <pre style={{ margin: 0, fontSize: 12, color: tc.textSecondary, whiteSpace: "pre-wrap" }}>
            {JSON.stringify(lastDecision, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}

function panelStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    padding: 14,
    borderRadius: 10,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
  } as const;
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontSize: 12,
    fontFamily: "inherit",
    boxSizing: "border-box" as const,
  };
}

function labelStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    display: "flex",
    flexDirection: "column" as const,
    gap: 6,
    fontSize: 12,
    color: tc.textMuted,
  };
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
