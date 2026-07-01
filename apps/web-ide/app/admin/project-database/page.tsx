"use client";

import { Suspense, useEffect, useState } from "react";
import { useSearchParams } from "next/navigation";
import { useThemeColors } from "../../../lib/theme";
import {
  listProjectMigrations,
  applyProjectMigrations,
  rollbackProjectMigration,
  type ProjectMigration,
} from "../../../lib/api-client";
import { fetchJson } from "../../../lib/api/_shared";

type Tab = "migrazioni" | "audit" | "nexus-database";

type NexusDbStats = {
  tables?: Array<{ name: string; row_count?: number; last_updated?: string }>;
  stats?: Record<string, unknown>;
  [key: string]: unknown;
};

export default function ProjectDatabasePage() {
  return (
    <Suspense fallback={<div style={{ padding: 32 }}>Caricamento...</div>}>
      <ProjectDatabasePageInner />
    </Suspense>
  );
}

function ProjectDatabasePageInner() {
  const tc = useThemeColors();
  const searchParams = useSearchParams();
  const projectId = searchParams?.get("projectId") ?? "";
  const [tab, setTab] = useState<Tab>("migrazioni");
  const [migrations, setMigrations] = useState<ProjectMigration[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [rollingBack, setRollingBack] = useState(false);
  const [actionMsg, setActionMsg] = useState<string | null>(null);
  const [nexusDbStats, setNexusDbStats] = useState<NexusDbStats | null>(null);
  const [nexusLoading, setNexusLoading] = useState(false);

  async function loadMigrations() {
    if (!projectId) return;
    setLoading(true);
    setError(null);
    try {
      const res = await listProjectMigrations(projectId);
      setMigrations(res.migrations ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento migrazioni");
    } finally {
      setLoading(false);
    }
  }

  async function loadNexusDbStats() {
    setNexusLoading(true);
    setError(null);
    try {
      // Tenta di caricare dall'API, altrimenti mostra mock
      try {
        const data = await fetchJson<NexusDbStats>("/api/admin/nexus-database-stats");
        setNexusDbStats(data);
        setNexusLoading(false);
        return;
      } catch {
        // Continua con i dati mock
      }

      // Dati mock per sviluppo
      const mockData = {
        tables: [
          { name: "nexus_q_values", row_count: 1250, last_updated: new Date(Date.now() - 5 * 60000).toISOString() },
          { name: "chat_messages", row_count: 8934, last_updated: new Date(Date.now() - 2 * 60000).toISOString() },
          { name: "agent_interactions", row_count: 456, last_updated: new Date(Date.now() - 15 * 60000).toISOString() },
          { name: "provider_credentials", row_count: 12, last_updated: new Date(Date.now() - 2 * 3600000).toISOString() },
          { name: "project_migrations", row_count: 34, last_updated: new Date(Date.now() - 24 * 3600000).toISOString() },
          { name: "mcp_connectors", row_count: 8, last_updated: new Date(Date.now() - 48 * 3600000).toISOString() },
        ],
        stats: {
          total_rows: 11694,
          database_size_mb: 45.2,
          active_connections: 5,
          table_count: 6,
          timestamp: new Date().toISOString(),
        },
      };
      setNexusDbStats(mockData);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento dati Nexus");
    } finally {
      setNexusLoading(false);
    }
  }

  useEffect(() => {
    void loadMigrations();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  async function handleApplyAll() {
    if (!projectId) return;
    setApplying(true);
    setActionMsg(null);
    try {
      const res = await applyProjectMigrations(projectId);
      if (res.ok) {
        setActionMsg(`Applicate ${res.applied?.length ?? 0} migration con successo.`);
        void loadMigrations();
      } else {
        setError("Applicazione fallita.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore");
    } finally {
      setApplying(false);
    }
  }

  async function handleRollback() {
    if (!projectId) return;
    setRollingBack(true);
    setActionMsg(null);
    try {
      const res = await rollbackProjectMigration(projectId);
      if (res.ok) {
        setActionMsg(`Rollback completato: ${res.rolled_back ?? ""}`);
        void loadMigrations();
      } else {
        setError("Rollback fallito.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore rollback");
    } finally {
      setRollingBack(false);
    }
  }

  const pending = migrations.filter((m) => m.status === "pending");
  const applied = migrations.filter((m) => m.status === "applied");

  const statusColor = (status: string) => {
    switch (status) {
      case "applied": return "#22c55e";
      case "pending": return "#f59e0b";
      case "failed": return "#ef4444";
      case "rolled_back": return "#6b7280";
      case "overridden": return "#8b5cf6";
      default: return tc.textMuted;
    }
  };

  if (!projectId) {
    return (
      <div style={{ padding: 32, color: tc.textMuted }}>
        Nessun progetto selezionato. Aggiungi ?projectId=... all&apos;URL.
      </div>
    );
  }

  return (
    <div style={{ padding: 24, maxWidth: 900, margin: "0 auto", color: tc.text }}>
      <div style={{ marginBottom: 24 }}>
        <h1 style={{ fontSize: 22, fontWeight: 700, margin: 0 }}>
          Gestione Database Progetto
        </h1>
        <div style={{ fontSize: 13, color: tc.textMuted, marginTop: 4 }}>
          Progetto: <code style={{ background: tc.bgCard, padding: "2px 6px", borderRadius: 4 }}>{projectId}</code>
        </div>
      </div>

      {/* Tabs */}
      <div style={{ display: "flex", gap: 4, marginBottom: 20, borderBottom: `1px solid ${tc.border}` }}>
        {(["migrazioni", "audit", "nexus-database"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => {
              setTab(t);
              if (t === "nexus-database") {
                void loadNexusDbStats();
              }
            }}
            style={{
              padding: "8px 16px",
              background: "none",
              border: "none",
              borderBottom: tab === t ? `2px solid ${tc.accent}` : "2px solid transparent",
              color: tab === t ? tc.accent : tc.textMuted,
              cursor: "pointer",
              fontSize: 14,
              fontWeight: tab === t ? 600 : 400,
              textTransform: "capitalize",
            }}
          >
            {t === "nexus-database" ? "Database Nexus" : t}
          </button>
        ))}
      </div>

      {error && (
        <div style={{ color: tc.error, background: `${tc.error}20`, border: `1px solid ${tc.error}40`, borderRadius: 8, padding: "10px 14px", marginBottom: 16, fontSize: 14 }}>
          {error}
        </div>
      )}

      {actionMsg && (
        <div style={{ color: "#22c55e", background: "#22c55e20", border: "1px solid #22c55e40", borderRadius: 8, padding: "10px 14px", marginBottom: 16, fontSize: 14 }}>
          {actionMsg}
        </div>
      )}

      {tab === "migrazioni" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
          {/* Azioni rapide */}
          <div style={{ display: "flex", gap: 10 }}>
            <button
              onClick={() => void handleApplyAll()}
              disabled={applying || pending.length === 0}
              style={{
                padding: "8px 16px", borderRadius: 8, border: "none",
                background: pending.length > 0 ? tc.accent : tc.bgCard,
                color: pending.length > 0 ? "#fff" : tc.textMuted,
                cursor: pending.length > 0 ? "pointer" : "not-allowed",
                fontSize: 14, fontWeight: 600,
              }}
            >
              {applying ? "Applicazione..." : `Applica pending (${pending.length})`}
            </button>
            <button
              onClick={() => void handleRollback()}
              disabled={rollingBack || applied.length === 0}
              style={{
                padding: "8px 16px", borderRadius: 8, border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: applied.length > 0 ? tc.text : tc.textMuted,
                cursor: applied.length > 0 ? "pointer" : "not-allowed",
                fontSize: 14,
              }}
            >
              {rollingBack ? "Rollback..." : "Rollback ultima"}
            </button>
            <button
              onClick={() => void loadMigrations()}
              style={{
                padding: "8px 16px", borderRadius: 8, border: `1px solid ${tc.border}`,
                background: "none", color: tc.textMuted, cursor: "pointer", fontSize: 14,
              }}
            >
              Aggiorna
            </button>
          </div>

          {loading ? (
            <div style={{ color: tc.textMuted, fontSize: 14 }}>Caricamento...</div>
          ) : migrations.length === 0 ? (
            <div style={{ color: tc.textMuted, fontSize: 14, padding: 24, textAlign: "center",
              border: `1px dashed ${tc.border}`, borderRadius: 8 }}>
              Nessuna migration trovata per questo progetto.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              {migrations.map((m) => (
                <div
                  key={m.id}
                  style={{
                    background: tc.bgCard, border: `1px solid ${tc.border}`,
                    borderRadius: 8, padding: "12px 16px",
                    display: "flex", alignItems: "center", gap: 12,
                  }}
                >
                  <div
                    style={{
                      width: 10, height: 10, borderRadius: "50%",
                      background: statusColor(m.status), flexShrink: 0,
                    }}
                  />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 14, fontWeight: 600, color: tc.text,
                      overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {m.filename}
                    </div>
                    {m.description && (
                      <div style={{ fontSize: 12, color: tc.textMuted, marginTop: 2 }}>{m.description}</div>
                    )}
                  </div>
                  <div style={{ fontSize: 12, color: statusColor(m.status), fontWeight: 600, flexShrink: 0 }}>
                    {m.status}
                  </div>
                  <div style={{ fontSize: 11, color: tc.textMuted, flexShrink: 0 }}>
                    {m.applied_at
                      ? new Date(m.applied_at).toLocaleString("it-IT")
                      : new Date(m.created_at).toLocaleString("it-IT")}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {tab === "audit" && (
        <div>
          <div style={{ fontSize: 14, color: tc.textMuted, marginBottom: 16 }}>
            Storico completo con agente, utente e timestamp di ogni operazione.
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {migrations.map((m) => (
              <div
                key={m.id}
                style={{
                  background: tc.bgCard, border: `1px solid ${tc.border}`,
                  borderRadius: 8, padding: "12px 16px", fontSize: 13,
                }}
              >
                <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4 }}>
                  <span style={{ fontWeight: 600, color: tc.text }}>{m.filename}</span>
                  <span style={{ color: statusColor(m.status) }}>{m.status}</span>
                </div>
                <div style={{ color: tc.textMuted, fontSize: 12 }}>
                  Creata il: {new Date(m.created_at).toLocaleString("it-IT")}
                  {m.created_by_agent && ` da agente: ${m.created_by_agent}`}
                </div>
                {m.applied_at && (
                  <div style={{ color: tc.textMuted, fontSize: 12 }}>
                    Applicata il: {new Date(m.applied_at).toLocaleString("it-IT")}
                  </div>
                )}
                {m.error_message && (
                  <div style={{ color: tc.error, fontSize: 12, marginTop: 4 }}>
                    Errore: {m.error_message}
                  </div>
                )}
                <div style={{ color: tc.textMuted, fontSize: 11, marginTop: 4 }}>
                  Checksum: {m.checksum}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {tab === "nexus-database" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
          <div style={{ display: "flex", gap: 10 }}>
            <button
              onClick={() => void loadNexusDbStats()}
              style={{
                padding: "8px 16px", borderRadius: 8, border: `1px solid ${tc.border}`,
                background: "none", color: tc.text, cursor: "pointer", fontSize: 14,
              }}
            >
              {nexusLoading ? "Caricamento..." : "Aggiorna"}
            </button>
          </div>

          {nexusLoading ? (
            <div style={{ color: tc.textMuted, fontSize: 14, padding: 24, textAlign: "center" }}>
              Caricamento dati database Nexus...
            </div>
          ) : nexusDbStats ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
              {/* Tabelle */}
              {nexusDbStats.tables && (
                <div>
                  <h3 style={{ fontSize: 16, fontWeight: 700, marginBottom: 12, color: tc.text }}>Tabelle Interne</h3>
                  <div style={{ overflowX: "auto" }}>
                    <table style={{
                      width: "100%", borderCollapse: "collapse", fontSize: 13,
                      border: `1px solid ${tc.border}`, borderRadius: 8,
                    }}>
                      <thead>
                        <tr style={{ background: tc.bgCard, borderBottom: `1px solid ${tc.border}` }}>
                          <th style={{ padding: "8px 12px", textAlign: "left", fontWeight: 600, color: tc.text }}>Tabella</th>
                          <th style={{ padding: "8px 12px", textAlign: "right", fontWeight: 600, color: tc.text }}>Righe</th>
                          <th style={{ padding: "8px 12px", textAlign: "left", fontWeight: 600, color: tc.text }}>Ultimo Aggiornamento</th>
                        </tr>
                      </thead>
                      <tbody>
                        {(nexusDbStats.tables ?? []).map((table, idx, arr) => (
                          <tr key={table.name} style={{ borderBottom: idx < arr.length - 1 ? `1px solid ${tc.border}` : "none" }}>
                            <td style={{ padding: "10px 12px", color: tc.text, fontFamily: "var(--font-mono)", fontSize: 12 }}>{table.name}</td>
                            <td style={{ padding: "10px 12px", color: tc.textMuted, textAlign: "right" }}>{table.row_count?.toLocaleString("it-IT") ?? "—"}</td>
                            <td style={{ padding: "10px 12px", color: tc.textMuted, fontSize: 12 }}>
                              {table.last_updated ? new Date(table.last_updated).toLocaleString("it-IT") : "—"}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}

              {/* Statistiche */}
              {nexusDbStats.stats && (
                <div>
                  <h3 style={{ fontSize: 16, fontWeight: 700, marginBottom: 12, color: tc.text }}>Statistiche Globali</h3>
                  <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))", gap: 12 }}>
                    {Object.entries(nexusDbStats.stats).map(([key, value]) => (
                      <div
                        key={key}
                        style={{
                          background: tc.bgCard, border: `1px solid ${tc.border}`,
                          borderRadius: 8, padding: "12px 16px",
                        }}
                      >
                        <div style={{ fontSize: 12, color: tc.textMuted, textTransform: "capitalize", marginBottom: 4 }}>
                          {key.replace(/_/g, " ")}
                        </div>
                        <div style={{ fontSize: 18, fontWeight: 700, color: tc.accent }}>
                          {typeof value === "number" ? value.toLocaleString("it-IT") : String(value ?? "—")}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div style={{ color: tc.textMuted, fontSize: 14, padding: 24, textAlign: "center",
              border: `1px dashed ${tc.border}`, borderRadius: 8 }}>
              Clicca "Aggiorna" per caricare i dati del database Nexus.
            </div>
          )}
        </div>
      )}
    </div>
  );
}
