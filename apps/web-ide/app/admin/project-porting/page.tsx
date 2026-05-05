"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import {
  portProjects,
  listAdminSettings,
  type PortDetail,
} from "../../../lib/api-client";

export default function ProjectPortingPage() {
  const tc = useThemeColors();
  const [oldBase, setOldBase] = useState("");
  const [newBase, setNewBase] = useState("");
  const [currentBase, setCurrentBase] = useState("");
  const [preview, setPreview] = useState<PortDetail[] | null>(null);
  const [result, setResult] = useState<{
    workspacesUpdated: number;
    repositoriesUpdated: number;
    projectsBaseRootUpdated: boolean;
  } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  // Carica il projects_base_root corrente
  const loadCurrentBase = useCallback(async () => {
    try {
      const res = await listAdminSettings();
      const setting = res.settings.find(
        (s) => s.key === "projects_base_root",
      );
      if (setting) {
        setCurrentBase(setting.value);
        setOldBase(setting.value);
      }
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    loadCurrentBase();
  }, [loadCurrentBase]);

  const runPreview = async () => {
    if (!oldBase.trim() || !newBase.trim()) return;
    setLoading(true);
    setError("");
    setResult(null);
    try {
      const res = await portProjects(oldBase.trim(), newBase.trim(), true);
      if (res.error) {
        setError(res.error);
        setPreview(null);
      } else {
        setPreview(res.details);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const runPort = async () => {
    if (!oldBase.trim() || !newBase.trim()) return;
    setLoading(true);
    setError("");
    try {
      const res = await portProjects(oldBase.trim(), newBase.trim(), false);
      if (res.error) {
        setError(res.error);
      } else {
        setResult({
          workspacesUpdated: res.workspacesUpdated,
          repositoriesUpdated: res.repositoriesUpdated,
          projectsBaseRootUpdated: res.projectsBaseRootUpdated,
        });
        setPreview(null);
        setCurrentBase(newBase.trim());
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const inputStyle: React.CSSProperties = {
    flex: 1,
    padding: "8px 12px",
    fontSize: 13,
    fontFamily: '"JetBrains Mono", monospace',
    background: tc.bgCard,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    outline: "none",
  };

  const btnStyle = (bg: string): React.CSSProperties => ({
    padding: "8px 20px",
    fontSize: 13,
    fontWeight: 600,
    background: bg,
    color: "#fff",
    border: "none",
    borderRadius: 6,
    cursor: loading ? "not-allowed" : "pointer",
    opacity: loading ? 0.6 : 1,
  });

  return (
    <div
      style={{
        padding: 24,
        maxWidth: 900,
        color: tc.text,
        fontFamily: '"Inter", sans-serif',
      }}
    >
      <h2 style={{ fontSize: 20, fontWeight: 700, marginBottom: 4 }}>
        Porting Progetti
      </h2>
      <p style={{ fontSize: 13, color: tc.textMuted, marginBottom: 20 }}>
        Aggiorna i path dei progetti nel database quando la directory di deploy
        viene spostata su un altro disco o percorso.
      </p>

      {/* Current base root */}
      <div
        style={{
          padding: "10px 14px",
          background: tc.bgCard,
          border: `1px solid ${tc.border}`,
          borderRadius: 8,
          marginBottom: 20,
          fontSize: 13,
        }}
      >
        <span style={{ color: tc.textMuted }}>Base root attuale: </span>
        <span style={{ fontFamily: '"JetBrains Mono", monospace' }}>
          {currentBase || "..."}
        </span>
      </div>

      {/* Inputs */}
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 10,
          marginBottom: 16,
        }}
      >
        <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
          <label
            style={{ width: 100, fontSize: 13, color: tc.textMuted, flexShrink: 0 }}
          >
            Vecchio path
          </label>
          <input
            style={inputStyle}
            value={oldBase}
            onChange={(e) => setOldBase(e.target.value)}
            placeholder="/opt/ai-orchestrator/projects"
          />
        </div>
        <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
          <label
            style={{ width: 100, fontSize: 13, color: tc.textMuted, flexShrink: 0 }}
          >
            Nuovo path
          </label>
          <input
            style={inputStyle}
            value={newBase}
            onChange={(e) => setNewBase(e.target.value)}
            placeholder="/var/lib/postgresql/wal/nexus/projects"
          />
        </div>
      </div>

      {/* Buttons */}
      <div style={{ display: "flex", gap: 10, marginBottom: 20 }}>
        <button
          style={btnStyle(tc.accent)}
          onClick={runPreview}
          disabled={loading}
        >
          Anteprima
        </button>
        {preview && preview.length > 0 && (
          <button
            style={btnStyle("#d97706")}
            onClick={runPort}
            disabled={loading}
          >
            Applica Porting
          </button>
        )}
      </div>

      {/* Error */}
      {error && (
        <div
          style={{
            padding: "10px 14px",
            background: "#ef44441a",
            border: "1px solid #ef4444",
            borderRadius: 8,
            color: "#ef4444",
            fontSize: 13,
            marginBottom: 16,
          }}
        >
          {error}
        </div>
      )}

      {/* Success */}
      {result && (
        <div
          style={{
            padding: "10px 14px",
            background: "#22c55e1a",
            border: "1px solid #22c55e",
            borderRadius: 8,
            color: "#22c55e",
            fontSize: 13,
            marginBottom: 16,
          }}
        >
          Porting completato: {result.workspacesUpdated} workspace,{" "}
          {result.repositoriesUpdated} repository aggiornati.
          {result.projectsBaseRootUpdated && " Setting projects_base_root aggiornato."}
          <br />
          <span style={{ fontSize: 12, color: tc.textMuted }}>
            Riavvia il backend per applicare le modifiche alla cache.
          </span>
        </div>
      )}

      {/* Preview table */}
      {preview && preview.length > 0 && (
        <div
          style={{
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            overflow: "hidden",
          }}
        >
          <table
            style={{
              width: "100%",
              borderCollapse: "collapse",
              fontSize: 12,
              fontFamily: '"JetBrains Mono", monospace',
            }}
          >
            <thead>
              <tr style={{ background: tc.bgSidebar }}>
                <th
                  style={{
                    padding: "8px 12px",
                    textAlign: "left",
                    color: tc.textMuted,
                    fontWeight: 600,
                    borderBottom: `1px solid ${tc.border}`,
                  }}
                >
                  Tabella
                </th>
                <th
                  style={{
                    padding: "8px 12px",
                    textAlign: "left",
                    color: tc.textMuted,
                    fontWeight: 600,
                    borderBottom: `1px solid ${tc.border}`,
                  }}
                >
                  Vecchio path
                </th>
                <th
                  style={{
                    padding: "8px 12px",
                    textAlign: "left",
                    color: tc.textMuted,
                    fontWeight: 600,
                    borderBottom: `1px solid ${tc.border}`,
                  }}
                >
                  Nuovo path
                </th>
              </tr>
            </thead>
            <tbody>
              {preview.map((d, i) => (
                <tr
                  key={`${d.table}-${d.id}`}
                  style={{
                    background: i % 2 === 0 ? "transparent" : tc.bgCard,
                  }}
                >
                  <td
                    style={{
                      padding: "6px 12px",
                      borderBottom: `1px solid ${tc.border}`,
                      color: tc.accent,
                    }}
                  >
                    {d.table}
                  </td>
                  <td
                    style={{
                      padding: "6px 12px",
                      borderBottom: `1px solid ${tc.border}`,
                      color: "#ef4444",
                      wordBreak: "break-all",
                    }}
                  >
                    {d.oldPath}
                  </td>
                  <td
                    style={{
                      padding: "6px 12px",
                      borderBottom: `1px solid ${tc.border}`,
                      color: "#22c55e",
                      wordBreak: "break-all",
                    }}
                  >
                    {d.newPath}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {preview && preview.length === 0 && (
        <div
          style={{
            padding: "10px 14px",
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            color: tc.textMuted,
            fontSize: 13,
          }}
        >
          Nessun path da aggiornare con questo prefisso.
        </div>
      )}
    </div>
  );
}
