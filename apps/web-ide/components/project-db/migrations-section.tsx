"use client";

import type { Theme } from "../../lib/theme";
import type { ProjectMigration } from "../../lib/api-client";

interface Props {
  tc: Theme;
  showNewMig: boolean;
  isConfigured: boolean;
  migForm: { sql: string; reason: string };
  setMigForm: (updater: (f: { sql: string; reason: string }) => { sql: string; reason: string }) => void;
  busy: boolean;
  loading: boolean;
  error: string | null;
  actionMsg: string | null;
  migrations: ProjectMigration[];
  pending: ProjectMigration[];
  applied: ProjectMigration[];
  schemaCandidates: string[];
  selectedSchemaFile: string;
  setSelectedSchemaFile: (v: string) => void;
  statusColor: (s: string) => string;
  onCreateMigration: () => void;
  onApply: () => void;
  onRollback: () => void;
  onImportSchema: (filePath?: string) => void;
}

/** Blocco migrazioni: nuova migrazione, Applica/Rollback, import schema, lista migrazioni. */
export function MigrationsSection({
  tc,
  showNewMig,
  isConfigured,
  migForm,
  setMigForm,
  busy,
  loading,
  error,
  actionMsg,
  migrations,
  pending,
  applied,
  schemaCandidates,
  selectedSchemaFile,
  setSelectedSchemaFile,
  statusColor,
  onCreateMigration,
  onApply,
  onRollback,
  onImportSchema,
}: Props) {
  return (
    <>
      {showNewMig && isConfigured && (
        <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}`, display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 10, color: tc.textMuted }}>SQL</label>
          <textarea
            value={migForm.sql}
            onChange={(e) => setMigForm((f) => ({ ...f, sql: e.target.value }))}
            rows={6}
            placeholder="CREATE TABLE ..."
            style={{ padding: 6, fontSize: 11, fontFamily: "var(--font-mono)", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, resize: "vertical" }}
          />
          <label style={{ fontSize: 10, color: tc.textMuted }}>Motivo (min 10 caratteri)</label>
          <input
            type="text"
            value={migForm.reason}
            onChange={(e) => setMigForm((f) => ({ ...f, reason: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <button
            type="button"
            onClick={onCreateMigration}
            disabled={busy}
            style={{
              padding: "6px 8px",
              borderRadius: 6,
              border: "none",
              background: tc.accent,
              color: "#fff",
              cursor: busy ? "not-allowed" : "pointer",
              fontSize: 12,
              fontWeight: 600,
            }}
          >
            {busy ? "…" : "Crea ed esegui"}
          </button>
        </div>
      )}

      <div style={{ padding: 10, display: "flex", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
        <button
          type="button"
          onClick={onApply}
          disabled={busy || pending.length === 0}
          title="Applica migrazioni pending"
          style={{
            flex: 1,
            padding: "6px 8px",
            borderRadius: 6,
            border: "none",
            background: pending.length > 0 ? tc.accent : tc.bgCard,
            color: pending.length > 0 ? "#fff" : tc.textMuted,
            cursor: pending.length > 0 && !busy ? "pointer" : "not-allowed",
            fontSize: 12,
            fontWeight: 600,
          }}
        >
          {busy ? "…" : `Applica (${pending.length})`}
        </button>
        <button
          type="button"
          onClick={onRollback}
          disabled={busy || applied.length === 0}
          title="Rollback ultima"
          style={{
            flex: 1,
            padding: "6px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: applied.length > 0 ? tc.text : tc.textMuted,
            cursor: applied.length > 0 && !busy ? "pointer" : "not-allowed",
            fontSize: 12,
          }}
        >
          Rollback
        </button>
      </div>

      <div style={{ padding: "14px 10px 12px", marginTop: 4, display: "flex", flexDirection: "column", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
        <button
          type="button"
          onClick={() => onImportSchema(selectedSchemaFile || undefined)}
          disabled={busy}
          title="Cerca un file schema nel progetto ed eseguilo sul database"
          style={{
            width: "100%",
            padding: "8px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: tc.text,
            cursor: busy ? "not-allowed" : "pointer",
            fontSize: 12,
            fontWeight: 600,
            textAlign: "center",
          }}
        >
          {busy ? "…" : "Importa schema dai file"}
        </button>
        {schemaCandidates.length > 0 && (
          <div style={{ display: "flex", gap: 6 }}>
            <select
              value={selectedSchemaFile}
              onChange={(e) => setSelectedSchemaFile(e.target.value)}
              style={{
                flex: 1,
                padding: "6px 8px",
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: tc.text,
                fontSize: 11,
              }}
            >
              {schemaCandidates.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
            <button
              type="button"
              onClick={() => onImportSchema(selectedSchemaFile)}
              disabled={busy || !selectedSchemaFile}
              style={{
                padding: "6px 10px",
                borderRadius: 6,
                border: "none",
                background: tc.accent,
                color: "#fff",
                cursor: busy ? "not-allowed" : "pointer",
                fontSize: 11,
                fontWeight: 600,
              }}
            >
              Importa
            </button>
          </div>
        )}
      </div>

      <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: 10, display: "flex", flexDirection: "column", gap: 6 }}>
        {error && (
          <div
            style={{
              color: tc.error,
              background: `${tc.error}20`,
              border: `1px solid ${tc.error}40`,
              borderRadius: 6,
              padding: "6px 8px",
              fontSize: 11,
            }}
          >
            {error}
          </div>
        )}
        {actionMsg && (
          <div
            style={{
              color: "#22c55e",
              background: "#22c55e20",
              border: "1px solid #22c55e40",
              borderRadius: 6,
              padding: "6px 8px",
              fontSize: 11,
            }}
          >
            {actionMsg}
          </div>
        )}

        {loading ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>Caricamento…</div>
        ) : migrations.length === 0 ? (
          <div
            style={{
              color: tc.textMuted,
              fontSize: 12,
              padding: 14,
              textAlign: "center",
              border: `1px dashed ${tc.border}`,
              borderRadius: 6,
            }}
          >
            Nessuna migrazione trovata.
          </div>
        ) : (
          migrations.map((m) => (
            <div
              key={m.id}
              style={{
                background: tc.bgCard,
                border: `1px solid ${tc.border}`,
                borderRadius: 6,
                padding: "8px 10px",
                display: "flex",
                alignItems: "center",
                gap: 8,
              }}
            >
              <span
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: statusColor(m.status),
                  flexShrink: 0,
                }}
              />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div
                  style={{
                    fontSize: 12,
                    fontWeight: 600,
                    color: tc.text,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={m.filename}
                >
                  {m.filename}
                </div>
                {m.description && (
                  <div
                    style={{
                      fontSize: 11,
                      color: tc.textMuted,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {m.description}
                  </div>
                )}
              </div>
              <span
                style={{
                  fontSize: 10,
                  color: statusColor(m.status),
                  fontWeight: 600,
                  textTransform: "uppercase",
                  letterSpacing: "0.04em",
                  flexShrink: 0,
                }}
              >
                {m.status}
              </span>
            </div>
          ))
        )}
      </div>
    </>
  );
}
